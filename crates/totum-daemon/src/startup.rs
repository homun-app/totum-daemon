//! Inizializzazione operativa del daemon (D3, passo P8; journal in D4).
//!
//! Collega i moduli nell'ordine del piano: la radice verificata e il lock
//! esclusivo arrivano dal chiamante; il modulo completa i soli file iniziali
//! mancanti di identità e registro, riclassifica la coerenza, apre
//! [`config::Store`] e infine il journal, prima della prontezza. Configurazione
//! e journal condividono lo stesso accesso alla radice bloccata: nessuno dei
//! due può sopravvivere al rilascio del lock e il lock resta acquisito finché
//! l'ultimo utilizzatore vive.
//!
//! L'inizializzazione di identità e registro resta esclusiva: avviene sul
//! [`LockedRoot`] unico, prima che l'accesso venga condiviso, con lo stesso
//! riferimento esclusivo degli incrementi precedenti.
//!
//! Non esistono qui classificatori, regole di persistenza o controlli di
//! sicurezza propri: ogni decisione, scrittura, recupero, migrazione e rifiuto
//! appartiene a `identity`, `config` e `journal`. Il modulo non apre socket,
//! non crea thread e non guarda lo standard input.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use crate::config::{ConfigError, Store};
use crate::identity::{IdentityError, InitError};
use crate::journal::{Journal, JournalError};
use crate::platform::LockedRoot;
use crate::{config, identity, journal};

/// Esito dell'inizializzazione operativa.
#[derive(Debug)]
pub struct Startup {
    store: Store,
    journal: Journal,
    identity: identity::Startup,
    config: config::Startup,
    journal_startup: journal::Startup,
    root: PathBuf,
}

impl Startup {
    /// Store della configurazione, che condivide radice e lock con il journal.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Journal aperto prima della prontezza, che trattiene radice e lock.
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Esito della prima inizializzazione di identità e registro.
    pub fn identity(&self) -> &identity::Startup {
        &self.identity
    }

    /// Esito dell'apertura della configurazione.
    pub fn config(&self) -> &config::Startup {
        &self.config
    }

    /// Esito dell'apertura del journal.
    pub fn journal_startup(&self) -> &journal::Startup {
        &self.journal_startup
    }

    /// Percorso della radice di stato ancorata.
    pub fn root(&self) -> &std::path::Path {
        &self.root
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
    /// Il journal non è stato aperto: rifiuto o errore bloccante prima della
    /// prontezza.
    Journal(JournalError),
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
            StartupError::Journal(error) => {
                write!(f, "apertura del journal fallita: {error}")
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
            StartupError::Journal(error) => Some(error),
        }
    }
}

/// Completa l'inizializzazione, apre configurazione e journal sotto lo stesso
/// lock.
///
/// Il chiamante deve aver già risolto, verificato e ancorato la radice e
/// acquisito il lock esclusivo con [`platform::lock`](crate::platform::lock):
/// prima di questa chiamata nessun percorso operativo legge o scrive lo stato.
/// La sequenza è:
///
/// 1. [`identity::initialize`] classifica identità, registro, configurazione e
///    backup e pubblica soltanto i file iniziali mancanti; richiede il
///    riferimento esclusivo, quindi due inizializzazioni concorrenti sulla
///    stessa radice non compilano;
/// 2. la coerenza viene riclassificata e deve risultare iniziale per entrambi i
///    componenti; un contenuto comparso nel frattempo senza rispettare il lock
///    è un rifiuto, mai una riscrittura;
/// 3. l'accesso unico viene condiviso e [`Store::open_shared`] applica la
///    propria decisione con la coerenza aggiornata, persistendo prima
///    esecuzione, recupero o migrazione;
/// 4. [`Journal::open`] classifica o inizializza il journal sullo stesso
///    accesso, prima che il daemon dichiari la prontezza. Un errore bloccante
///    produce una diagnosi e nessuna prontezza.
///
/// Un errore prima del punto di commit di una singola operazione garantisce che
/// quella generazione è intatta; gli esiti di commit già avvenuti sono riportati
/// da [`identity::Startup`], [`config::Startup`] e [`journal::Startup`], non da
/// un errore.
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

    let shared = locked.share();
    let (store, config_startup) =
        Store::open_shared(shared.clone(), coherence).map_err(StartupError::Configuration)?;
    let (journal, journal_startup) = Journal::open(shared).map_err(StartupError::Journal)?;

    Ok(Startup {
        store,
        journal,
        identity: identity_startup,
        config: config_startup,
        journal_startup,
        root,
    })
}
