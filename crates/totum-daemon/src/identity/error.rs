//! Errori tipizzati dell'identità e del registro dei controller.
//!
//! Ogni rifiuto nomina il file o il componente e la causa. Nessun errore
//! produce un valore predefinito, un'identità generata o una riparazione: in D3
//! l'unico stato ammesso è quello iniziale, e un contenuto diverso è un errore.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use super::Created;
use crate::config::{Component, ConfigError};
use crate::platform::PlatformError;

/// Rifiuto della classificazione o della prima inizializzazione.
#[derive(Debug)]
pub enum IdentityError {
    /// Lettura fallita dopo l'apertura verificata.
    Read { path: PathBuf, source: io::Error },
    /// La primitiva di piattaforma ha rifiutato collegamento, tipo,
    /// proprietario o protezioni.
    Protection(PlatformError),
    /// Il file supera il limite di lettura.
    TooLarge { path: PathBuf, limit: u64 },
    /// Lo schema non è quello compreso: in P7 non esiste alcuna migrazione dei
    /// file di identità, quindi ogni versione diversa è rifiutata.
    UnsupportedSchema { path: PathBuf, schema_version: u64 },
    /// Il documento non è un oggetto JSON valido con i soli campi previsti.
    InvalidDocument { path: PathBuf, detail: String },
    /// Il documento è valido ma non iniziale: in D3 non è ammesso e non viene
    /// azzerato né corretto.
    NonInitial {
        component: Component,
        detail: String,
    },
    /// Una configurazione esiste ma manca identità o registro.
    IncoherentState { component: Component },
    /// La classificazione della configurazione ha rifiutato lo stato.
    ConfigRefused(Box<ConfigError>),
    /// La serializzazione dei valori iniziali è fallita.
    Serialize { detail: String },
    /// Una scrittura, sincronizzazione o pubblicazione è fallita prima del
    /// punto di commit di quel file.
    Write {
        action: &'static str,
        source: PlatformError,
    },
    /// I nomi dei file temporanei sono esauriti per collisioni ripetute.
    TemporaryCollision { attempts: u32 },
    /// Il file di destinazione è comparso durante la creazione: non viene
    /// sovrascritto.
    AppearedDuringInitialization { path: PathBuf },
}

impl IdentityError {
    pub(super) fn write(action: &'static str, source: PlatformError) -> Self {
        IdentityError::Write { action, source }
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::Read { path, source } => {
                write!(f, "lettura fallita per {}: {source}", path.display())
            }
            IdentityError::Protection(error) => {
                write!(f, "file di identità rifiutato: {error}")
            }
            IdentityError::TooLarge { path, limit } => write!(
                f,
                "file di identità oltre il limite di {limit} byte: {}",
                path.display()
            ),
            IdentityError::UnsupportedSchema {
                path,
                schema_version,
            } => write!(
                f,
                "schema {schema_version} non compreso in {}: nessuna migrazione dei file di identità",
                path.display()
            ),
            IdentityError::InvalidDocument { path, detail } => {
                write!(f, "documento non valido in {}: {detail}", path.display())
            }
            IdentityError::NonInitial { component, detail } => write!(
                f,
                "{} in stato non iniziale: {detail}",
                component_label(*component)
            ),
            IdentityError::IncoherentState { component } => write!(
                f,
                "configurazione presente senza {}: stato incoerente",
                component_label(*component)
            ),
            IdentityError::ConfigRefused(error) => {
                write!(f, "classificazione della configurazione rifiutata: {error}")
            }
            IdentityError::Serialize { detail } => {
                write!(f, "serializzazione dei valori iniziali fallita: {detail}")
            }
            IdentityError::Write { action, source } => {
                write!(f, "scrittura non commessa durante {action}: {source}")
            }
            IdentityError::TemporaryCollision { attempts } => write!(
                f,
                "nessun nome temporaneo disponibile dopo {attempts} tentativi"
            ),
            IdentityError::AppearedDuringInitialization { path } => write!(
                f,
                "{} è comparso durante la creazione: nessuna sovrascrittura",
                path.display()
            ),
        }
    }
}

impl Error for IdentityError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            IdentityError::Read { source, .. } => Some(source),
            IdentityError::Protection(error) => Some(error),
            IdentityError::ConfigRefused(error) => Some(error.as_ref()),
            IdentityError::Write { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn component_label(component: Component) -> &'static str {
    match component {
        Component::Identity => "identità",
        Component::Registry => "registro dei controller",
    }
}

/// Guasto della prima inizializzazione, con i file già pubblicati.
///
/// La coppia non è atomica: se il primo file è stato pubblicato e il secondo
/// fallisce, `created` lo riporta e il riavvio completa soltanto i file
/// mancanti. Un `created` vuoto garantisce che nessuna scrittura è stata resa
/// visibile da questa chiamata.
#[derive(Debug)]
pub struct InitError {
    pub error: IdentityError,
    pub created: Vec<Created>,
}

impl InitError {
    pub(super) fn new(error: IdentityError, created: Vec<Created>) -> Self {
        Self { error, created }
    }
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)?;
        if !self.created.is_empty() {
            let names: Vec<&str> = self.created.iter().map(Created::file_name).collect();
            write!(
                f,
                "; file iniziali già pubblicati prima del guasto: {}",
                names.join(", ")
            )?;
        }
        Ok(())
    }
}

impl Error for InitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}
