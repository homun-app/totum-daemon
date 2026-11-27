//! Errori tipizzati della piattaforma.
//!
//! Nessun errore viene convertito in un valore predefinito: una protezione non
//! verificabile produce un errore esplicito con il percorso e la causa.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

/// Errore della piattaforma, senza panici e senza ripieghi permissivi.
#[derive(Debug)]
pub enum PlatformError {
    /// Il sistema operativo non è supportato da questo modulo.
    UnsupportedPlatform,
    /// Il processo gira con privilegi elevati: la radice non viene preparata.
    ElevatedExecution,
    /// Una variabile d'ambiente necessaria alla risoluzione è assente.
    MissingEnvironment { variable: &'static str },
    /// Una variabile d'ambiente necessaria alla risoluzione non è valida.
    InvalidEnvironment {
        variable: &'static str,
        reason: &'static str,
    },
    /// Il percorso nominale non è utilizzabile come radice di stato.
    InvalidPath { path: PathBuf, reason: &'static str },
    /// Un componente del percorso è un collegamento simbolico.
    SymlinkRefused { path: PathBuf },
    /// Un componente del percorso è un reparse point di Windows.
    ReparsePointRefused { path: PathBuf },
    /// Il percorso esiste ma non è una directory.
    NotADirectory { path: PathBuf },
    /// Il percorso esiste ma non è un file regolare.
    NotARegularFile { path: PathBuf },
    /// Il percorso esiste ma non è un collegamento simbolico.
    NotASymlink { path: PathBuf },
    /// Il proprietario non è quello atteso.
    WrongOwner {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    /// La modalità o la DACL effettiva non è quella attesa.
    WrongMode {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    /// Un ACL concede diritti incompatibili con l'isolamento.
    AclViolation { path: PathBuf, detail: String },
    /// Una protezione non può essere letta o interpretata.
    UnverifiableProtection { path: PathBuf, detail: String },
    /// Il percorso non coincide più con la directory ancorata.
    PathChanged { path: PathBuf },
    /// Il file richiesto non esiste nella radice verificata.
    ///
    /// È distinto da [`PlatformError::PathChanged`]: la radice e il percorso
    /// sono stati verificati e l'unica condizione osservata è l'assenza del
    /// file. Non viene prodotto per una directory o per un componente del
    /// percorso mancante, che restano un cambio di percorso.
    FileAbsent { path: PathBuf },
    /// L'oggetto da creare esiste già ed è trattato come preesistente.
    AlreadyExists { path: PathBuf },
    /// Un'altra istanza detiene già il lock esclusivo della radice.
    ///
    /// È l'esito tipizzato della seconda istanza: nessuna attesa, nessuna
    /// rimozione del file e nessuna interpretazione del suo contenuto.
    AlreadyRunning { path: PathBuf },
    /// Errore del sistema operativo.
    Io { path: PathBuf, source: io::Error },
}

impl PlatformError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: impl Into<io::Error>) -> Self {
        PlatformError::Io {
            path: path.into(),
            source: source.into(),
        }
    }

    pub(crate) fn invalid(path: impl Into<PathBuf>, reason: &'static str) -> Self {
        PlatformError::InvalidPath {
            path: path.into(),
            reason,
        }
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlatformError::UnsupportedPlatform => write!(f, "piattaforma non supportata"),
            PlatformError::ElevatedExecution => write!(
                f,
                "esecuzione elevata rifiutata: la radice di stato non viene preparata con privilegi amministrativi"
            ),
            PlatformError::MissingEnvironment { variable } => {
                write!(f, "variabile d'ambiente {variable} assente")
            }
            PlatformError::InvalidEnvironment { variable, reason } => {
                write!(f, "variabile d'ambiente {variable} non valida: {reason}")
            }
            PlatformError::InvalidPath { path, reason } => {
                write!(f, "percorso non valido {}: {reason}", path.display())
            }
            PlatformError::SymlinkRefused { path } => {
                write!(f, "collegamento simbolico rifiutato: {}", path.display())
            }
            PlatformError::ReparsePointRefused { path } => {
                write!(f, "reparse point rifiutato: {}", path.display())
            }
            PlatformError::NotADirectory { path } => {
                write!(f, "attesa una directory: {}", path.display())
            }
            PlatformError::NotARegularFile { path } => {
                write!(f, "atteso un file regolare: {}", path.display())
            }
            PlatformError::NotASymlink { path } => {
                write!(f, "atteso un collegamento simbolico: {}", path.display())
            }
            PlatformError::WrongOwner {
                path,
                expected,
                actual,
            } => write!(
                f,
                "proprietario errato per {}: atteso {expected}, trovato {actual}",
                path.display()
            ),
            PlatformError::WrongMode {
                path,
                expected,
                actual,
            } => write!(
                f,
                "modalità errata per {}: attesa {expected:04o}, trovata {actual:04o}",
                path.display()
            ),
            PlatformError::AclViolation { path, detail } => {
                write!(
                    f,
                    "protezione ACL non valida per {}: {detail}",
                    path.display()
                )
            }
            PlatformError::UnverifiableProtection { path, detail } => {
                write!(
                    f,
                    "protezione non verificabile per {}: {detail}",
                    path.display()
                )
            }
            PlatformError::PathChanged { path } => write!(
                f,
                "il percorso {} non coincide più con la directory verificata",
                path.display()
            ),
            PlatformError::FileAbsent { path } => {
                write!(
                    f,
                    "file assente nella radice verificata: {}",
                    path.display()
                )
            }
            PlatformError::AlreadyExists { path } => {
                write!(f, "l'oggetto esiste già: {}", path.display())
            }
            PlatformError::AlreadyRunning { path } => write!(
                f,
                "un'altra istanza detiene il lock della radice: {}",
                path.display()
            ),
            PlatformError::Io { path, source } => {
                write!(f, "errore di sistema su {}: {source}", path.display())
            }
        }
    }
}

impl Error for PlatformError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            PlatformError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
