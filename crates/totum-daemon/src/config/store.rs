//! Apertura classificata, prima creazione, recupero, migrazione e salvataggi
//! serializzati.
//!
//! [`Store::open`] applica la decisione della classificazione: rifiuta ciò che
//! il piano non ammette, crea la revisione 0 alla prima esecuzione, persiste il
//! recupero dal backup quando è ammesso, migra in avanti una generazione con
//! schema precedente e scarta i temporanei del namespace riservato.
//! [`Store::save`] rivalida lo stato su disco prima di scrivere e serializza i
//! salvataggi della stessa istanza.
//!
//! La costruzione richiede [`LockedRoot`], l'oggetto che possiede insieme la
//! radice ancorata e il lock esclusivo acquisito con
//! [`platform::lock`](crate::platform::lock), e lo conserva per tutta la vita
//! dello `Store`: nessuna classificazione operativa, pulizia, migrazione o
//! scrittura avviene senza il lock esclusivo di **quella** radice, e il guard
//! non può essere rilasciato mentre lo `Store` resta utilizzabile. Poiché il
//! lock non è un valore separato, non esiste un modo per associarlo a una
//! radice diversa.

use std::sync::{Mutex, PoisonError};

use super::error::ConfigError;
use super::write::{self, Commit};
use super::{
    BackupState, CONFIG_FILE, CoherenceInput, ConfigV1, Decision, GenerationFailure,
    MigrationSource, classify, document, read_bounded,
};
use crate::platform::LockedRoot;

/// Classificazioni massime per un'apertura: una collisione in prima creazione
/// richiede una nuova classificazione, senza cicli illimitati.
const MAX_OPEN_ATTEMPTS: u32 = 3;

/// Come è stata ottenuta la generazione corrente all'apertura.
#[derive(Debug)]
pub enum StartupKind {
    /// Nessuna configurazione esisteva: `config.json` è stato creato a
    /// revisione 0.
    FirstRun { commit: Commit },
    /// `config.json` era valido ed è stato caricato senza modifiche.
    Normal { backup: BackupState },
    /// Il principale era assente o corrotto e il backup valido è stato
    /// persistito come nuova generazione principale.
    Recovered {
        backup: ConfigV1,
        main_failure: GenerationFailure,
        /// Nome della copia del principale corrotto conservata per diagnostica.
        preserved: Option<std::ffi::OsString>,
        commit: Commit,
    },
    /// Una generazione con schema precedente è stata trasformata in v1,
    /// incrementando la revisione, e persistita. Il backup conserva i byte
    /// originali della sorgente.
    Migrated {
        /// File da cui è partita la migrazione.
        source: MigrationSource,
        /// Versione dello schema della sorgente.
        from_schema_version: u64,
        /// Revisione della generazione precedente.
        previous_revision: u64,
        /// Copia del principale corrotto, quando la sorgente era il backup.
        preserved: Option<std::ffi::OsString>,
        commit: Commit,
    },
}

/// Esito dell'apertura, con la generazione caricata e ciò che è stato scritto.
#[derive(Debug)]
pub struct Startup {
    pub kind: StartupKind,
    /// Temporanei propri rimossi prima dell'apertura.
    pub discarded_temporaries: Vec<std::ffi::OsString>,
}

/// Configurazione aperta su una radice verificata, sotto il lock di istanza.
///
/// Conserva la revisione caricata e la radice bloccata ottenuta con
/// [`platform::lock`](crate::platform::lock); ogni salvataggio rivalida i due
/// file di configurazione su disco. Identità e registro non vengono riletti:
/// la loro classificazione appartiene a P7.
#[derive(Debug)]
pub struct Store {
    // Posseduto per tutta la vita dello `Store`: la radice e il lock sono un
    // solo oggetto e non esiste un modo per rilasciare il lock separatamente.
    locked: LockedRoot,
    current: Mutex<ConfigV1>,
}

impl Store {
    /// Classifica e applica la decisione, senza collegarsi all'avvio dei
    /// binari.
    ///
    /// `locked` è la radice verificata e ancorata con il proprio lock
    /// esclusivo, ottenuta da [`platform::lock`](crate::platform::lock) dopo
    /// [`platform::prepare_root`](crate::platform::prepare_root): una seconda
    /// istanza si arresta con `AlreadyRunning` prima di leggere o toccare la
    /// configurazione. Il lock appartiene alla radice ancorata, quindi non può
    /// essere trasferito a un'altra directory con lo stesso percorso nominale.
    /// Lo `Store` trattiene l'oggetto finché resta vivo. La coerenza di identità
    /// e registro è un input tipizzato del chiamante. Nessuna scrittura avviene
    /// se la decisione è un rifiuto. Una migrazione riconosciuta viene
    /// applicata mantenendo il lock per l'intera operazione: la sorgente
    /// indicata dalla classificazione viene riletta, trasformata e persistita
    /// con le garanzie della scrittura atomica.
    pub fn open(
        locked: LockedRoot,
        coherence: CoherenceInput,
    ) -> Result<(Store, Startup), ConfigError> {
        let root = locked.verified_root();
        let collision_path = root.path().join(CONFIG_FILE);
        for _ in 0..MAX_OPEN_ATTEMPTS {
            match classify(root, coherence) {
                Decision::Refused(error) => return Err(error),
                Decision::MigrationRequired {
                    from_schema_version,
                    source,
                    main_failure,
                } => {
                    // La sorgente viene riletta sotto lo stesso lock: se non è
                    // più quella classificata, la migrazione non viene
                    // applicata.
                    let original = match read_bounded(root, source.file_name()) {
                        Ok(Some(bytes)) => bytes,
                        Ok(None) => return Err(ConfigError::MigrationSourceChanged { source }),
                        Err(error) => return Err(error),
                    };
                    let previous = match document::parse(&original) {
                        document::Parsed::Migratable(previous) => previous,
                        _ => return Err(ConfigError::MigrationSourceChanged { source }),
                    };
                    let next = document::migrate_v0_to_v1(previous)?;
                    let discarded = write::discard_reserved_temporaries(root)?;
                    let (commit, preserved) =
                        write::migrate(root, source, &original, next, main_failure.as_ref())?;
                    return Ok((
                        Store::with_config(locked, next),
                        Startup {
                            kind: StartupKind::Migrated {
                                source,
                                from_schema_version,
                                previous_revision: previous.revision(),
                                preserved,
                                commit,
                            },
                            discarded_temporaries: discarded,
                        },
                    ));
                }
                Decision::FirstRun { config } => {
                    let discarded = write::discard_reserved_temporaries(root)?;
                    match write::create_initial(root, config) {
                        Ok(commit) => {
                            return Ok((
                                Store::with_config(locked, config),
                                Startup {
                                    kind: StartupKind::FirstRun { commit },
                                    discarded_temporaries: discarded,
                                },
                            ));
                        }
                        // La destinazione è comparsa: si torna alla
                        // classificazione e non si sovrascrive nulla.
                        Err(ConfigError::AppearedDuringFirstRun { .. }) => continue,
                        Err(error) => return Err(error),
                    }
                }
                Decision::Normal { config, backup } => {
                    let discarded = write::discard_reserved_temporaries(root)?;
                    return Ok((
                        Store::with_config(locked, config),
                        Startup {
                            kind: StartupKind::Normal { backup },
                            discarded_temporaries: discarded,
                        },
                    ));
                }
                Decision::RecoveryRequired { backup, main } => {
                    let discarded = write::discard_reserved_temporaries(root)?;
                    let (commit, preserved) = write::recover(root, backup, &main)?;
                    return Ok((
                        Store::with_config(locked, backup),
                        Startup {
                            kind: StartupKind::Recovered {
                                backup,
                                main_failure: main,
                                preserved,
                                commit,
                            },
                            discarded_temporaries: discarded,
                        },
                    ));
                }
            }
        }
        Err(ConfigError::AppearedDuringFirstRun {
            path: collision_path,
        })
    }

    fn with_config(locked: LockedRoot, config: ConfigV1) -> Self {
        Self {
            locked,
            current: Mutex::new(config),
        }
    }

    /// Generazione corrente, senza accedere al disco.
    pub fn config(&self) -> ConfigV1 {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Salva la generazione successiva e restituisce l'esito del commit.
    ///
    /// Prima di scrivere, i due file su disco vengono riclassificati: schema
    /// futuro, protezioni o tipo errati, principale assente o corrotto,
    /// migrazione necessaria e revisione regressiva producono lo stesso
    /// rifiuto dell'apertura. Se la revisione del principale non è quella
    /// caricata, l'esito è [`ConfigError::RevisionDivergence`] e la generazione
    /// divergente non viene adottata né sovrascritta; un principale da
    /// recuperare produce [`ConfigError::RecoveryRequired`], perché il
    /// recupero resta un percorso esplicito e non viene applicato qui.
    ///
    /// Un `Err` garantisce che nulla è stato commesso: la generazione
    /// precedente è intatta e il chiamante non deve ritentare da solo. Un
    /// `Commit::CommittedNotDurable` è invece un salvataggio riuscito con
    /// durabilità non confermata, e neppure quello va ritentato.
    pub fn save(&self) -> Result<Commit, ConfigError> {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        let commit = write::save(self.locked.verified_root(), *current)?;
        *current = ConfigV1::with_revision(commit.revision());
        Ok(commit)
    }
}
