//! Inizializzazione operativa del daemon (D3, passo P8).
//!
//! Collega i moduli dei passi precedenti nell'ordine del piano: la radice
//! verificata e il lock esclusivo arrivano dal chiamante, poi il modulo completa
//! i soli file iniziali mancanti di identità e registro, riclassifica la
//! coerenza e apre [`config::Store`] con quella coerenza aggiornata. Lo `Store`
//! e il lock restano di proprietà dell'esito finché il processo vive.
//!
//! Non esistono qui classificatori, regole di persistenza o controlli di
//! sicurezza propri: ogni decisione, scrittura, recupero, migrazione e rifiuto
//! appartiene a `identity` e `config`. Il modulo non apre socket, non crea
//! thread e non guarda lo standard input.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use crate::config::{ConfigError, Store};
use crate::identity::{IdentityError, InitError};
use crate::platform::LockedRoot;
use crate::{config, identity};

/// Esito dell'inizializzazione operativa.
#[derive(Debug)]
pub struct Startup {
    store: Store,
    identity: identity::Startup,
    config: config::Startup,
    root: PathBuf,
}

impl Startup {
    /// Store della configurazione, che trattiene radice e lock.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Esito della prima inizializzazione di identità e registro.
    pub fn identity(&self) -> &identity::Startup {
        &self.identity
    }

    /// Esito dell'apertura della configurazione.
    pub fn config(&self) -> &config::Startup {
        &self.config
    }

    /// Percorso della radice di stato ancorata.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Consuma l'esito e restituisce lo `Store`, che continua a trattenere il
    /// lock per tutta la sua vita.
    pub fn into_store(self) -> Store {
        self.store
    }
}

/// Guasto dell'inizializzazione operativa.
///
/// Ogni variante incapsula l'errore del modulo che ha rifiutato l'operazione;
/// nessuna produce uno stato alternativo o una riparazione. Un errore di
/// inizializzazione di identità riporta i file già pubblicati prima del guasto.
#[derive(Debug)]
pub enum StartupError {
    /// Identità e registro non sono stati completati.
    IdentityInitialization(InitError),
    /// Dopo l'inizializzazione la classificazione non conferma due componenti
    /// presenti e iniziali.
    IdentityVerification(IdentityError),
    /// La configurazione non è stata aperta: rifiuto, recupero o migrazione
    /// falliti prima del punto di commit.
    Configuration(ConfigError),
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartupError::IdentityInitialization(error) => write!(f, "{error}"),
            StartupError::IdentityVerification(error) => write!(
                f,
                "verifica dell'identità dopo l'inizializzazione fallita: {error}"
            ),
            StartupError::Configuration(error) => {
                write!(f, "apertura della configurazione fallita: {error}")
            }
        }
    }
}

impl Error for StartupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            StartupError::IdentityInitialization(error) => Some(error),
            StartupError::IdentityVerification(error) => Some(error),
            StartupError::Configuration(error) => Some(error),
        }
    }
}

/// Completa l'inizializzazione e apre la configurazione sotto lo stesso lock.
///
/// Il chiamante deve aver già risolto, verificato e ancorato la radice e
/// acquisito il lock esclusivo con [`platform::lock`](crate::platform::lock):
/// prima di questa chiamata nessun percorso operativo legge o scrive lo stato.
/// La sequenza è:
///
/// 1. [`identity::initialize`] classifica identità, registro, configurazione e
///    backup e pubblica soltanto i file iniziali mancanti;
/// 2. la coerenza viene riclassificata e deve risultare iniziale per entrambi i
///    componenti; un contenuto comparso nel frattempo senza rispettare il lock
///    è un rifiuto, mai una riscrittura;
/// 3. [`Store::open`] applica la propria decisione con la coerenza aggiornata,
///    persistendo prima esecuzione, recupero o migrazione sotto lo stesso lock.
///
/// Un errore prima del punto di commit di una singola operazione garantisce che
/// quella generazione è intatta; gli esiti di commit già avvenuti sono riportati
/// da [`identity::Startup`] e [`config::Startup`], non da un errore.
pub fn open(mut locked: LockedRoot) -> Result<Startup, StartupError> {
    let root = locked.path().to_path_buf();

    let identity_startup =
        identity::initialize(&mut locked).map_err(StartupError::IdentityInitialization)?;

    let classification =
        identity::classify(locked.verified_root()).map_err(StartupError::IdentityVerification)?;
    let coherence = classification.coherence();
    classification
        .require_initial()
        .map_err(StartupError::IdentityVerification)?;

    let (store, config_startup) =
        Store::open(locked, coherence).map_err(StartupError::Configuration)?;

    Ok(Startup {
        store,
        identity: identity_startup,
        config: config_startup,
        root,
    })
}
