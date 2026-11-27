//! Configurazione versionata (D3, passi P3, P4, P5 e P6).
//!
//! Il modulo espone lo schema, i valori iniziali, la validazione rigorosa, la
//! classificazione di `config.json` e `config.json.prev`, la persistenza (prima
//! creazione a revisione 0, salvataggi con incremento controllato, backup della
//! generazione corrente, sostituzione atomica, recupero persistito e gestione
//! dei temporanei), il requisito del lock di istanza — [`Store`] si costruisce
//! soltanto con il [`LockedRoot`](crate::platform::LockedRoot) di
//! [`platform::lock`](crate::platform::lock), che possiede insieme la radice
//! ancorata e il lock e lo trattiene fino alla sua chiusura — e, dal passo P6,
//! le migrazioni soltanto in avanti: uno schema precedente valido viene
//! trasformato e persistito con le stesse garanzie di P4, conservando i byte
//! originali nel backup. Non è collegato all'avvio dei binari.
//!
//! Regole:
//!
//! - i file si leggono e si scrivono soltanto attraverso le primitive
//!   verificate di [`platform`](crate::platform): nessun collegamento seguito,
//!   nessun controllo di proprietà, tipo e protezioni aggirato;
//! - l'assenza di un file è riconosciuta soltanto dall'esito dedicato
//!   `FileAbsent`; un cambio di percorso o un errore di sistema producono un
//!   rifiuto e non diventano mai prima esecuzione, recupero o migrazione;
//! - il percorso della radice viene ricontrollato prima della lettura: una
//!   rinomina o una sostituzione è un rifiuto;
//! - la lettura è limitata a [`MAX_CONFIG_BYTES`] e un file più grande è un
//!   errore, mai un caricamento illimitato;
//! - un campo sconosciuto, un tipo errato, un campo duplicato o un documento
//!   troncato rendono il file inutilizzabile; non esistono valori predefiniti
//!   silenziosi; un documento che non sia un oggetto JSON (array, scalare o
//!   `null`) è corrotto e non viene mai interpretato, nemmeno come sorgente di
//!   migrazione;
//! - uno schema futuro non viene reinterpretato come configurazione corrente;
//! - uno schema precedente viene migrato soltanto in avanti, verso lo schema
//!   compreso, con una trasformazione pura e un salvataggio che conserva i byte
//!   originali nel backup; una sorgente riconosciuta ma non migrabile è un
//!   rifiuto, mai una reinterpretazione o un recupero;
//! - la classificazione non modifica contenuto o presenza dei file; soltanto
//!   [`Store::open`] applica prima creazione, recupero, migrazione e pulizia dei
//!   temporanei del namespace riservato, e soltanto dopo una decisione che lo
//!   ammette;
//! - ogni salvataggio rivalida i due file su disco prima di creare temporanei:
//!   schema futuro, protezioni errate, principale corrotto, stato incompatibile
//!   o revisione diversa da quella attesa producono un errore e nessuna
//!   scrittura; il recupero non viene mai applicato da un salvataggio;
//! - dopo la prima creazione `config.json` non è mai assente: la nuova
//!   generazione viene scritta in un temporaneo e pubblicata con una rinomina
//!   atomica;
//! - [`Store`] si costruisce soltanto con `LockedRoot`, che possiede insieme la
//!   radice e il lock esclusivo, e lo trattiene per tutta la propria vita: una
//!   seconda istanza cooperante viene rifiutata prima di leggere o toccare la
//!   configurazione. Il lock è consultivo e coordina le istanze di Totum, non
//!   software che lo ignora.

use std::ffi::OsStr;
use std::io::Read;
use std::path::PathBuf;

use crate::platform::{PlatformError, VerifiedRoot};

mod classify;
mod document;
mod error;
mod store;
mod write;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod write_tests;

pub use error::ConfigError;
pub use store::{Startup, StartupKind, Store};
pub use write::Commit;

/// Nome del file di configurazione corrente nella radice di stato.
pub const CONFIG_FILE: &str = "config.json";
/// Nome della generazione precedente, usata per il recupero.
pub const CONFIG_BACKUP_FILE: &str = "config.json.prev";
/// Versione dello schema compresa da questo incremento.
pub const CONFIG_SCHEMA_VERSION: u64 = 1;
/// Schema precedente, esclusivamente sintetico, ammesso come sorgente di una
/// migrazione in avanti.
///
/// Non è un formato storico reale: nessuna versione distribuita ha mai scritto
/// lo schema 0. Serve alla fixture `v0 → v1` per provare il meccanismo.
pub const MIGRATABLE_SCHEMA_VERSION: u64 = 0;
/// Limite di lettura di un file di configurazione: 64 KiB.
pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// Documento di configurazione dello schema v1.
///
/// Contiene soltanto l'intestazione: `schemaVersion` è sempre
/// [`CONFIG_SCHEMA_VERSION`] e `revision` è l'intero monotono della generazione.
/// I campi di dominio arriveranno con gli incrementi che li introducono.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigV1 {
    revision: u64,
}

impl ConfigV1 {
    /// Valori iniziali della prima esecuzione, tenuti soltanto in memoria.
    pub fn initial() -> Self {
        Self { revision: 0 }
    }

    /// Versione dello schema del documento.
    pub fn schema_version(&self) -> u64 {
        CONFIG_SCHEMA_VERSION
    }

    /// Revisione della generazione.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn with_revision(revision: u64) -> Self {
        Self { revision }
    }
}

/// Documento di configurazione dello schema v0 sintetico.
///
/// Contiene soltanto `schemaVersion: 0`: nessun campo di dominio e nessuna
/// `revision`. La revisione è implicitamente 0, perché lo schema 0 non è mai
/// stato distribuito e non ha una storia di generazioni da conservare. Non è
/// un formato storico reale: esiste soltanto per provare la migrazione in
/// avanti verso [`ConfigV1`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigV0 {
    revision: u64,
}

impl ConfigV0 {
    /// Valore di uno schema 0 valido: la revisione è sempre 0.
    pub fn initial() -> Self {
        Self { revision: 0 }
    }

    /// Versione dello schema del documento.
    pub fn schema_version(&self) -> u64 {
        MIGRATABLE_SCHEMA_VERSION
    }

    /// Revisione della generazione.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Costruttore delle prove: lo schema 0 reale porta sempre revisione 0,
    /// quindi un valore diverso esiste soltanto per provare l'overflow.
    #[cfg(test)]
    pub(crate) fn with_revision(revision: u64) -> Self {
        Self { revision }
    }
}

/// Componente di stato che partecipa alla coerenza dell'avvio.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Component {
    /// Stato dell'identità (`identity.json`).
    Identity,
    /// Contenitore del registro dei controller (`controllers.json`).
    Registry,
}

impl Component {
    fn label(self) -> &'static str {
        match self {
            Component::Identity => "identità",
            Component::Registry => "registro dei controller",
        }
    }
}

/// Stato di un componente di stato, fornito dal modulo che lo classifica.
///
/// Il modulo `config` non legge né interpreta i file di identità e registro:
/// riceve uno stato già tipizzato e lo usa solo per la coerenza dell'avvio.
/// La classificazione dei contenuti di quei file appartiene al passo P7.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComponentState {
    /// File assente.
    Absent,
    /// File presente e valido con contenuto iniziale.
    Initial,
    /// File presente e valido con contenuto non iniziale; non ammesso in D3.
    NonInitial,
    /// File presente ma inutilizzabile: corrotto, schema futuro, tipo,
    /// proprietario o protezioni errate. La classificazione spetta al modulo
    /// del componente; qui produce un arresto senza scritture.
    Unusable,
}

impl ComponentState {
    fn label(self) -> &'static str {
        match self {
            ComponentState::Absent => "assente",
            ComponentState::Initial => "iniziale",
            ComponentState::NonInitial => "non iniziale",
            ComponentState::Unusable => "inutilizzabile",
        }
    }
}

/// Input espliciti di coerenza per identità e registro.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoherenceInput {
    pub identity: ComponentState,
    pub registry: ComponentState,
}

/// Stato della generazione precedente quando il principale è valido.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackupState {
    /// Nessuna generazione precedente.
    Absent,
    /// Generazione precedente valida, con revisione uguale o inferiore.
    Available { revision: u64 },
    /// Generazione precedente illeggibile: segnalata e non riparata.
    Corrupt { detail: String },
    /// Generazione precedente con schema più vecchio: riservata, non caricata
    /// mentre il principale è valido. Non viene migrata al posto del
    /// principale: una migrazione parte dal backup soltanto quando il
    /// principale è assente o corrotto.
    OlderSchema { schema_version: u64 },
}

/// Guasto della generazione principale.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GenerationFailure {
    /// Il file principale è assente.
    Absent,
    /// Il file principale non è interpretabile come schema v1.
    Corrupt { detail: String },
}

/// Generazione da cui parte una migrazione in avanti.
///
/// La provenienza è esplicita perché principale e backup possono avere schemi
/// diversi: senza di essa, migrare il file sbagliato sarebbe un'ambiguità.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MigrationSource {
    /// `config.json`, il principale.
    Main,
    /// `config.json.prev`, il backup, ammesso soltanto quando il principale è
    /// assente o corrotto.
    Backup,
}

impl MigrationSource {
    /// Nome del file che contiene la generazione da migrare.
    pub fn file_name(self) -> &'static str {
        match self {
            MigrationSource::Main => CONFIG_FILE,
            MigrationSource::Backup => CONFIG_BACKUP_FILE,
        }
    }

    fn label(self) -> &'static str {
        match self {
            MigrationSource::Main => "principale",
            MigrationSource::Backup => "backup",
        }
    }
}

/// Decisione unica della classificazione, in sola lettura.
#[derive(Debug)]
pub enum Decision {
    /// Principale valido: si usa quello. Il backup è assente, valido o
    /// illeggibile; in quest'ultimo caso è segnalato e non riparato.
    Normal {
        config: ConfigV1,
        backup: BackupState,
    },
    /// Nessun file di configurazione esiste e le condizioni di prima
    /// esecuzione sono soddisfatte. I valori iniziali restano in memoria:
    /// la persistenza appartiene a P4.
    FirstRun { config: ConfigV1 },
    /// Il principale è assente o corrotto e il backup è valido: il recupero è
    /// necessario, ma P3 non lo applica.
    RecoveryRequired {
        backup: ConfigV1,
        main: GenerationFailure,
    },
    /// La generazione da caricare usa uno schema più vecchio e riconosciuto:
    /// serve una migrazione in avanti, applicata da [`Store::open`] sotto il
    /// lock. `source` indica quale file va trasformato e `main_failure` è il
    /// guasto del principale quando la sorgente è il backup.
    MigrationRequired {
        from_schema_version: u64,
        source: MigrationSource,
        main_failure: Option<GenerationFailure>,
    },
    /// Stato incompatibile: nessuna scrittura, nessun recupero, nessuna
    /// ricreazione dei valori iniziali.
    Refused(ConfigError),
}

/// Classifica `config.json` e `config.json.prev` senza modificare nulla.
///
/// La radice deve essere già verificata e ancorata da
/// [`platform::prepare_root`](crate::platform::prepare_root). Prima della
/// lettura il percorso viene ricontrollato: una rinomina o una sostituzione
/// della radice produce `PathChanged` e un rifiuto. I due file vengono aperti
/// con [`VerifiedRoot::open_file`], che rifiuta collegamenti simbolici e
/// verifica tipo, proprietario, modalità e ACL. Un file assente nella radice
/// verificata produce `FileAbsent` e viene classificato come assente; nessun
/// altro errore viene interpretato come assenza. La lettura è limitata a
/// [`MAX_CONFIG_BYTES`].
///
/// Ordine di precedenza dei rifiuti: percorso cambiato, schema futuro,
/// protezioni o tipo errati (compresi errori di lettura e dimensione oltre il
/// limite), revisione non valida o in diminuzione, stato non iniziale o
/// inutilizzabile di identità e registro, configurazione presente senza
/// identità o registro, principale con schema precedente al backup. Poi:
/// principale valido, recupero, migrazione, prima esecuzione.
///
/// Uno schema precedente è riconosciuto soltanto se il suo contenuto è
/// esattamente quello dello schema sintetico 0; un contenuto diverso è
/// corrotto e non viene mai migrato. La migrazione parte dal principale
/// quando è valido; dal backup soltanto quando il principale è assente o
/// corrotto. Un backup con schema precedente accanto a un principale valido
/// resta di riserva.
///
/// Un temporaneo residuo (`.config.<pid>.<contatore>.tmp`) non viene letto né
/// applicato; la classificazione non lo rimuove, lo scarta soltanto
/// [`Store::open`] dopo una decisione che ammette la scrittura.
pub fn classify(root: &VerifiedRoot, coherence: CoherenceInput) -> Decision {
    classify::classify(root, coherence)
}

/// Voce osservata di un singolo file di configurazione.
pub(super) enum Observed {
    Absent,
    Current(ConfigV1),
    /// Schema precedente riconosciuto e migrabile.
    Old(ConfigV0),
    Corrupt {
        detail: String,
    },
    TooNew {
        path: PathBuf,
        schema_version: u64,
    },
    Refused(ConfigError),
}

/// Legge e classifica un file con le primitive verificate della piattaforma.
///
/// L'assenza è riconosciuta soltanto da [`PlatformError::FileAbsent`], che le
/// due implementazioni producono quando il file richiesto non esiste nella
/// radice verificata. Un cambio di percorso o un errore di sistema non viene
/// mai interpretato come assenza.
pub(super) fn observe(root: &VerifiedRoot, name: &str) -> Observed {
    let buffer = match read_bounded(root, name) {
        Ok(Some(buffer)) => buffer,
        Ok(None) => return Observed::Absent,
        Err(error) => return Observed::Refused(error),
    };

    match document::parse(&buffer) {
        document::Parsed::Current(config) => Observed::Current(config),
        document::Parsed::Migratable(config) => Observed::Old(config),
        document::Parsed::TooNew { schema_version } => Observed::TooNew {
            path: root.path().join(name),
            schema_version,
        },
        document::Parsed::Invalid { detail } => Observed::Corrupt { detail },
        document::Parsed::InvalidRevision { detail } => {
            Observed::Refused(ConfigError::InvalidRevision {
                path: root.path().join(name),
                detail,
            })
        }
    }
}

/// Legge un file della radice con le primitive verificate, entro il limite.
///
/// `Ok(None)` è l'assenza del file richiesto rispetto alla radice verificata;
/// ogni altro esito diverso da `Ok(Some(_))` è un rifiuto esplicito: un cambio
/// di percorso, un tipo, un proprietario o protezioni errate, un errore di
/// sistema o un file oltre [`MAX_CONFIG_BYTES`].
pub(super) fn read_bounded(
    root: &VerifiedRoot,
    name: &str,
) -> Result<Option<Vec<u8>>, ConfigError> {
    let path = root.path().join(name);
    let file = match root.open_file(OsStr::new(name)) {
        Ok(file) => file,
        Err(PlatformError::FileAbsent { .. }) => return Ok(None),
        Err(error) => return Err(ConfigError::Protection(error)),
    };

    // `take` limita la lettura: il buffer non può superare il limite anche se
    // il file è molto più grande o cresce durante la lettura.
    let mut buffer = Vec::new();
    let mut reader = file.take(MAX_CONFIG_BYTES + 1);
    reader
        .read_to_end(&mut buffer)
        .map_err(|source| ConfigError::Read { path, source })?;
    if u64::try_from(buffer.len()).unwrap_or(u64::MAX) > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge {
            path: root.path().join(name),
            limit: MAX_CONFIG_BYTES,
        });
    }
    Ok(Some(buffer))
}
