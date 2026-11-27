//! Identità e registro dei controller (D3, passo P7).
//!
//! Il modulo implementa soltanto lo stato persistito iniziale dell'identità e il
//! contenitore vuoto del registro: `identity.json` contiene
//! `{"schemaVersion":1,"state":"notInitialized"}` e `controllers.json` contiene
//! `{"schemaVersion":1,"grantRevision":0,"controllers":[]}`. In D3 non esistono
//! chiavi, impronte, token, voci di controller, pairing, autorizzazione,
//! modifica dei grant o `resetIdentity`: il modulo non genera alcun materiale
//! crittografico e non definisce lo schema delle voci future.
//!
//! Regole:
//!
//! - i due file si leggono soltanto attraverso le primitive verificate di
//!   [`platform`](crate::platform), con un limite di [`MAX_IDENTITY_BYTES`] per
//!   file; un collegamento, un tipo, un proprietario o una protezione errati
//!   sono rifiuti e non vengono riparati;
//! - l'assenza è riconosciuta soltanto dall'esito dedicato
//!   [`PlatformError::FileAbsent`]; nessun altro errore diventa assenza;
//! - un documento deve essere un oggetto JSON con tipi esatti: campi
//!   sconosciuti, duplicati, mancanti o troncati e schemi diversi da 1 sono
//!   rifiutati; una versione diversa da 1 non viene mai migrata in P7;
//! - un registro non vuoto, una `grantRevision` diversa da zero o uno stato
//!   diverso da `notInitialized` sono contenuti validi ma non iniziali: non
//!   vengono azzerati, corretti né reinterpretati;
//! - la classificazione è in sola lettura e produce stati tipizzati per
//!   [`config::CoherenceInput`](crate::config::CoherenceInput), mantenendo
//!   distinta l'assenza da un file iniziale presente;
//! - la prima inizializzazione richiede un accesso esclusivo al [`LockedRoot`]
//!   (`&mut`), scrive soltanto i file mancanti con temporanei protetti e
//!   pubblicazione senza sovrascrittura, dopo aver classificato identità,
//!   registro, configurazione e backup; non crea la configurazione;
//! - l'accesso esclusivo è garantito dall'API: due chiamate concorrenti sulla
//!   stessa radice non compilano, mentre radici diverse restano indipendenti;
//!   la pulizia dei temporanei non può quindi cancellare il temporaneo attivo
//!   di un'altra chiamata;
//! - i due file non sono una coppia atomica: la pubblicazione di ciascuno è il
//!   proprio punto di commit e un arresto fra i due lascia uno stato parziale
//!   che il riavvio completa.

use std::ffi::OsStr;
use std::io::Read;

use crate::config::{self, CoherenceInput, Component, ComponentState, Decision};
use crate::platform::{LockedRoot, PlatformError, VerifiedRoot};

mod document;
mod error;
mod write;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod write_tests;

pub use error::{IdentityError, InitError};
pub use write::{Commit, Created};

/// Nome del file di stato dell'identità.
pub const IDENTITY_FILE: &str = "identity.json";
/// Nome del contenitore del registro dei controller.
pub const CONTROLLERS_FILE: &str = "controllers.json";
/// Versione dello schema compresa da questo incremento.
pub const IDENTITY_SCHEMA_VERSION: u64 = 1;
/// Versione dello schema del registro compresa da questo incremento.
pub const CONTROLLERS_SCHEMA_VERSION: u64 = 1;
/// Unico stato persistito ammesso in D3.
pub const INITIAL_STATE: &str = "notInitialized";
/// Revisione iniziale dei grant.
pub const INITIAL_GRANT_REVISION: u64 = 0;
/// Limite di lettura di un file di identità: 64 KiB.
pub const MAX_IDENTITY_BYTES: u64 = 64 * 1024;

/// Classificazioni massime per un'apertura: un oggetto comparso durante la
/// creazione richiede una nuova classificazione, senza cicli illimitati.
const MAX_INIT_ATTEMPTS: u32 = 3;

/// Stato persistito dell'identità.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    /// In D3 l'unico stato ammesso: nessuna identità crittografica è stata
    /// generata.
    NotInitialized,
}

impl State {
    /// Nome persistito dello stato.
    pub fn as_str(self) -> &'static str {
        INITIAL_STATE
    }
}

/// Stato iniziale caricato di identità e registro.
///
/// In D3 il registro è sempre vuoto e la revisione dei grant è zero: i campi
/// sono privati e non esiste un percorso che li modifichi.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Identity {
    state: State,
    grant_revision: u64,
    controller_count: usize,
}

impl Identity {
    /// Valori iniziali tenuti in memoria.
    pub fn initial() -> Self {
        Self {
            state: State::NotInitialized,
            grant_revision: INITIAL_GRANT_REVISION,
            controller_count: 0,
        }
    }

    /// Stato persistito dell'identità.
    pub fn state(&self) -> State {
        self.state
    }

    /// Revisione dei grant.
    pub fn grant_revision(&self) -> u64 {
        self.grant_revision
    }

    /// Numero di controller registrati; in D3 è sempre zero.
    pub fn controller_count(&self) -> usize {
        self.controller_count
    }

    /// Indica se lo stato è quello iniziale.
    pub fn is_initial(&self) -> bool {
        self.state == State::NotInitialized
            && self.grant_revision == INITIAL_GRANT_REVISION
            && self.controller_count == 0
    }
}

/// Esito osservato di un singolo file di identità o registro.
#[derive(Debug)]
pub enum Observation {
    /// Il file non esiste nella radice verificata.
    Absent,
    /// Il file esiste ed è valido con il contenuto iniziale.
    Initial,
    /// Il documento è valido ma non iniziale; non ammesso in D3.
    NonInitial { detail: String },
    /// Il file è inutilizzabile: corrotto, schema non compreso, tipo,
    /// proprietario o protezioni errate, oltre il limite di lettura.
    Unusable(IdentityError),
}

impl Observation {
    /// Stato equivalente per [`config::CoherenceInput`].
    pub fn component_state(&self) -> ComponentState {
        match self {
            Observation::Absent => ComponentState::Absent,
            Observation::Initial => ComponentState::Initial,
            Observation::NonInitial { .. } => ComponentState::NonInitial,
            Observation::Unusable(_) => ComponentState::Unusable,
        }
    }

    /// Indica se il file è assente.
    pub fn is_absent(&self) -> bool {
        matches!(self, Observation::Absent)
    }

    /// Indica se il file è presente e iniziale.
    pub fn is_initial(&self) -> bool {
        matches!(self, Observation::Initial)
    }
}

/// Classificazione in sola lettura di `identity.json` e `controllers.json`.
#[derive(Debug)]
pub struct Classification {
    pub identity: Observation,
    pub controllers: Observation,
}

impl Classification {
    /// Input di coerenza per il modulo `config`.
    ///
    /// L'assenza resta [`ComponentState::Absent`], distinta da un file
    /// iniziale presente: la configurazione rifiuta una propria presenza senza
    /// entrambi i componenti.
    pub fn coherence(&self) -> CoherenceInput {
        CoherenceInput {
            identity: self.identity.component_state(),
            registry: self.controllers.component_state(),
        }
    }

    /// Stato dell'identità per la configurazione.
    pub fn identity_state(&self) -> ComponentState {
        self.identity.component_state()
    }

    /// Stato del registro per la configurazione.
    pub fn registry_state(&self) -> ComponentState {
        self.controllers.component_state()
    }

    /// Richiede che entrambi i componenti siano presenti e iniziali.
    ///
    /// Consuma la classificazione: un file assente, non iniziale o
    /// inutilizzabile produce [`IdentityError`] con il componente interessato.
    /// Usata dopo [`initialize`] per confermare che lo stato osservato è
    /// quello atteso prima di aprire la configurazione.
    pub fn require_initial(self) -> Result<(), IdentityError> {
        require_presence(self.identity, Component::Identity)?;
        require_presence(self.controllers, Component::Registry)
    }
}

fn require_presence(observation: Observation, component: Component) -> Result<(), IdentityError> {
    match observation {
        Observation::Initial => Ok(()),
        Observation::Absent => Err(IdentityError::IncoherentState { component }),
        Observation::NonInitial { detail } => Err(IdentityError::NonInitial { component, detail }),
        Observation::Unusable(error) => Err(error),
    }
}

/// Esito della prima inizializzazione.
#[derive(Debug)]
pub struct Startup {
    /// Stato iniziale caricato dopo le scritture.
    pub identity: Identity,
    /// File pubblicati da questa chiamata, nell'ordine di creazione.
    pub created: Vec<Created>,
    /// Temporanei del namespace `.identity.` rimossi prima delle scritture.
    pub discarded_temporaries: Vec<std::ffi::OsString>,
}

/// Classifica i due file senza modificare nulla.
///
/// La radice deve essere già verificata e ancorata da
/// [`platform::prepare_root`](crate::platform::prepare_root). Il percorso viene
/// ricontrollato prima della lettura: una rinomina o una sostituzione produce
/// [`IdentityError::Protection`] e un rifiuto. Un file assente nella radice
/// verificata produce `FileAbsent` e diventa [`Observation::Absent`]; nessun
/// altro errore viene interpretato come assenza.
pub fn classify(root: &VerifiedRoot) -> Result<Classification, IdentityError> {
    root.verify_path().map_err(IdentityError::Protection)?;
    Ok(Classification {
        identity: observe_identity(root),
        controllers: observe_controllers(root),
    })
}

/// Inizializza i soli file mancanti sotto il lock esclusivo della radice.
///
/// L'accesso esclusivo è imposto dalla firma: `&mut LockedRoot` impedisce due
/// chiamate concorrenti sulla stessa radice, quindi la pulizia dei temporanei
/// non può cancellare il temporaneo attivo di un'altra chiamata. Radici diverse
/// restano indipendenti e non esiste un lock globale. Dopo l'inizializzazione il
/// `LockedRoot` può essere trasferito a `config::Store`.
///
/// ```compile_fail,E0308
/// use std::thread;
/// use totum_daemon::identity;
/// use totum_daemon::platform::LockedRoot;
///
/// fn initialize_concurrently(locked: &LockedRoot) {
///     thread::scope(|scope| {
///         scope.spawn(|| identity::initialize(locked));
///         scope.spawn(|| identity::initialize(locked));
///     });
/// }
/// ```
///
/// Prima di qualunque scrittura classifica identità, registro, configurazione e
/// backup. Le regole sono quelle della tabella di P7:
///
/// - entrambi i componenti iniziali: nessuna scrittura;
/// - uno o entrambi assenti e configurazione e backup assenti: crea soltanto i
///   file mancanti, prima `identity.json` e poi `controllers.json`;
/// - configurazione o backup presenti con un componente assente: rifiuto dello
///   stato incoerente, nessuna ricreazione parziale;
/// - un componente non iniziale o inutilizzabile, o una configurazione
///   inutilizzabile quando occorre scrivere: rifiuto senza modifiche.
///
/// La configurazione non viene mai creata da questo modulo. Ogni file viene
/// pubblicato senza sostituzione: se la destinazione compare durante la
/// creazione, la classificazione viene ripetuta al massimo [`MAX_INIT_ATTEMPTS`]
/// volte e nulla viene sovrascritto.
pub fn initialize(locked: &mut LockedRoot) -> Result<Startup, InitError> {
    let root = locked.verified_root();
    let mut created = Vec::new();
    let mut discarded_temporaries = Vec::new();
    let mut appeared: Option<std::path::PathBuf> = None;

    for _ in 0..MAX_INIT_ATTEMPTS {
        let classification = match classify(root) {
            Ok(classification) => classification,
            Err(error) => return Err(InitError::new(error, created)),
        };
        let identity_presence = match prepare(classification.identity, Component::Identity) {
            Ok(presence) => presence,
            Err(error) => return Err(InitError::new(error, created)),
        };
        let controllers_presence = match prepare(classification.controllers, Component::Registry) {
            Ok(presence) => presence,
            Err(error) => return Err(InitError::new(error, created)),
        };

        let coherence = CoherenceInput {
            identity: identity_presence.component_state(),
            registry: controllers_presence.component_state(),
        };
        let decision = config::classify(root, coherence);
        if matches!(&decision, Decision::Refused(_)) {
            let Decision::Refused(error) = decision else {
                unreachable!("il ramo di rifiuto è già stato verificato")
            };
            return Err(match error {
                crate::config::ConfigError::IncoherentState { component } => {
                    InitError::new(IdentityError::IncoherentState { component }, created)
                }
                other => InitError::new(IdentityError::ConfigRefused(Box::new(other)), created),
            });
        }

        if identity_presence == Presence::Initial && controllers_presence == Presence::Initial {
            match write::discard_temporaries(root) {
                Ok(discarded) => discarded_temporaries.extend(discarded),
                Err(error) => return Err(InitError::new(error, created)),
            }
            return Ok(Startup {
                identity: Identity::initial(),
                created,
                discarded_temporaries,
            });
        }

        if !matches!(decision, Decision::FirstRun { .. }) {
            let component = identity_presence
                .missing_component()
                .or_else(|| controllers_presence.missing_component())
                .unwrap_or(Component::Registry);
            return Err(InitError::new(
                IdentityError::IncoherentState { component },
                created,
            ));
        }

        match write::discard_temporaries(root) {
            Ok(discarded) => discarded_temporaries.extend(discarded),
            Err(error) => return Err(InitError::new(error, created)),
        }

        if identity_presence == Presence::Absent {
            let bytes = match document::serialize_identity() {
                Ok(bytes) => bytes,
                Err(error) => return Err(InitError::new(error, created)),
            };
            match write::create_missing(root, IDENTITY_FILE, &bytes) {
                Ok(file) => created.push(file),
                Err(IdentityError::AppearedDuringInitialization { path }) => {
                    appeared = Some(path);
                    continue;
                }
                Err(error) => return Err(InitError::new(error, created)),
            }
        }
        if controllers_presence == Presence::Absent {
            let bytes = match document::serialize_controllers() {
                Ok(bytes) => bytes,
                Err(error) => return Err(InitError::new(error, created)),
            };
            match write::create_missing(root, CONTROLLERS_FILE, &bytes) {
                Ok(file) => created.push(file),
                Err(IdentityError::AppearedDuringInitialization { path }) => {
                    appeared = Some(path);
                    continue;
                }
                Err(error) => return Err(InitError::new(error, created)),
            }
        }

        return Ok(Startup {
            identity: Identity::initial(),
            created,
            discarded_temporaries,
        });
    }

    Err(InitError::new(
        IdentityError::AppearedDuringInitialization {
            path: appeared.unwrap_or_else(|| root.path().join(IDENTITY_FILE)),
        },
        created,
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Presence {
    Absent,
    Initial,
}

impl Presence {
    fn component_state(self) -> ComponentState {
        match self {
            Presence::Absent => ComponentState::Absent,
            Presence::Initial => ComponentState::Initial,
        }
    }

    fn missing_component(self) -> Option<Component> {
        match self {
            Presence::Absent => Some(Component::Identity),
            Presence::Initial => None,
        }
    }
}

fn prepare(observation: Observation, component: Component) -> Result<Presence, IdentityError> {
    match observation {
        Observation::Absent => Ok(Presence::Absent),
        Observation::Initial => Ok(Presence::Initial),
        Observation::NonInitial { detail } => Err(IdentityError::NonInitial { component, detail }),
        Observation::Unusable(error) => Err(error),
    }
}

fn observe_identity(root: &VerifiedRoot) -> Observation {
    let path = root.path().join(IDENTITY_FILE);
    let bytes = match read_bounded(root, IDENTITY_FILE) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Observation::Absent,
        Err(error) => return Observation::Unusable(error),
    };
    match document::parse_identity(&bytes) {
        document::ParsedIdentity::Initial => Observation::Initial,
        document::ParsedIdentity::NonInitial { detail } => Observation::NonInitial { detail },
        document::ParsedIdentity::UnsupportedSchema { schema_version } => {
            Observation::Unusable(IdentityError::UnsupportedSchema {
                path,
                schema_version,
            })
        }
        document::ParsedIdentity::Invalid { detail } => {
            Observation::Unusable(IdentityError::InvalidDocument { path, detail })
        }
    }
}

fn observe_controllers(root: &VerifiedRoot) -> Observation {
    let path = root.path().join(CONTROLLERS_FILE);
    let bytes = match read_bounded(root, CONTROLLERS_FILE) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Observation::Absent,
        Err(error) => return Observation::Unusable(error),
    };
    match document::parse_controllers(&bytes) {
        document::ParsedControllers::Initial => Observation::Initial,
        document::ParsedControllers::NonInitial { detail } => Observation::NonInitial { detail },
        document::ParsedControllers::UnsupportedSchema { schema_version } => {
            Observation::Unusable(IdentityError::UnsupportedSchema {
                path,
                schema_version,
            })
        }
        document::ParsedControllers::Invalid { detail } => {
            Observation::Unusable(IdentityError::InvalidDocument { path, detail })
        }
    }
}

/// Legge un file della radice con le primitive verificate, entro il limite.
fn read_bounded(root: &VerifiedRoot, name: &str) -> Result<Option<Vec<u8>>, IdentityError> {
    let path = root.path().join(name);
    let file = match root.open_file(OsStr::new(name)) {
        Ok(file) => file,
        Err(PlatformError::FileAbsent { .. }) => return Ok(None),
        Err(error) => return Err(IdentityError::Protection(error)),
    };

    let mut buffer = Vec::new();
    let mut reader = file.take(MAX_IDENTITY_BYTES + 1);
    reader
        .read_to_end(&mut buffer)
        .map_err(|source| IdentityError::Read { path, source })?;
    if u64::try_from(buffer.len()).unwrap_or(u64::MAX) > MAX_IDENTITY_BYTES {
        return Err(IdentityError::TooLarge {
            path: root.path().join(name),
            limit: MAX_IDENTITY_BYTES,
        });
    }
    Ok(Some(buffer))
}
