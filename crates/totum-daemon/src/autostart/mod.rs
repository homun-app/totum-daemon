//! Avvio automatico per account (D3 P9).
//!
//! Il modulo offre tre primitive di libreria — stato in sola lettura,
//! registrazione idempotente e rimozione esplicita — per i soli oggetti di
//! Totum. Non avvia il daemon, non attiva la sessione corrente, non esegue
//! `launchctl`, `systemctl` o altri strumenti di sistema e non installa nulla
//! a livello di sistema.
//!
//! Ogni meccanismo è per account:
//!
//! - macOS: un Launch Agent in `~/Library/LaunchAgents`, con `RunAtLoad` e
//!   senza `KeepAlive`;
//! - Linux: un'unità systemd utente e un collegamento relativo in
//!   `default.target.wants`, creato dal codice senza invocare `systemctl`;
//! - Windows: un valore dedicato in
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, senza elevazione.
//!
//! La presenza della registrazione non dimostra che il daemon sia attivo, che
//! sia stato caricato o che partirà davvero al prossimo accesso. La verifica
//! di un accesso reale appartiene a un passo successivo e non è dichiarata da
//! questo modulo.
//!
//! Le scritture usano i file solo tramite le primitive ancorate di `platform`:
//! temporaneo con modalità 0600, sincronizzazione, pubblicazione che non
//! sostituisce o sostituzione atomica di un oggetto riconosciuto come proprio.
//! Un oggetto estraneo, un collegamento inatteso o una protezione errata non
//! vengono mai sovrascritti né rimossi. Le prove usano soltanto directory
//! temporanee tramite [`Layout`], mai il profilo reale.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::platform::PlatformError;

#[cfg(any(windows, test))]
mod command_line;
#[cfg(unix)]
mod plist;
#[cfg(any(windows, test))]
mod registry;
#[cfg(unix)]
mod unit;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// Meccanismo di avvio automatico.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mechanism {
    /// Launch Agent per l'utente (macOS).
    LaunchAgent,
    /// Unità systemd utente e collegamento `default.target.wants` (Linux).
    SystemdUser,
    /// Valore in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (Windows).
    RunKey,
}

impl Mechanism {
    /// Nome stabile usato in diagnostica e documentazione.
    pub fn name(self) -> &'static str {
        match self {
            Mechanism::LaunchAgent => "launch-agent",
            Mechanism::SystemdUser => "systemd-user",
            Mechanism::RunKey => "run-key",
        }
    }
}

/// Percorsi e disponibilità degli oggetti di avvio automatico.
///
/// Il valore predefinito deriva soltanto dal profilo dell'account. I
/// costruttori espliciti servono alle prove e non vengono usati dal binario:
/// puntano a directory temporanee, mai al profilo reale.
#[derive(Clone, Debug)]
pub struct Layout {
    mechanism: Mechanism,
    directory: PathBuf,
    #[cfg_attr(windows, allow(dead_code))]
    systemd_available: bool,
}

impl Layout {
    /// Rileva il layout del meccanismo appropriato per la piattaforma.
    ///
    /// Su Linux il meccanismo è considerato disponibile soltanto se esiste
    /// `/run/systemd/system`, la stessa condizione usata da `sd_booted()`. La
    /// disponibilità viene registrata nel layout al momento della costruzione;
    /// un cambiamento successivo del sistema non viene inseguito.
    pub fn for_current_user() -> Result<Self, AutostartError> {
        #[cfg(target_os = "macos")]
        {
            let home = environment_path("HOME")?;
            Ok(Layout {
                mechanism: Mechanism::LaunchAgent,
                directory: home.join("Library/LaunchAgents"),
                systemd_available: false,
            })
        }
        #[cfg(target_os = "linux")]
        {
            let base = match std::env::var_os("XDG_CONFIG_HOME") {
                Some(value) => {
                    let path = PathBuf::from(value);
                    if !path.is_absolute() {
                        return Err(AutostartError::InvalidEnvironment {
                            variable: "XDG_CONFIG_HOME",
                            reason: "il percorso non è assoluto",
                        });
                    }
                    path
                }
                None => environment_path("HOME")?.join(".config"),
            };
            Ok(Layout {
                mechanism: Mechanism::SystemdUser,
                directory: base.join("systemd/user"),
                systemd_available: Path::new("/run/systemd/system").is_dir(),
            })
        }
        #[cfg(windows)]
        {
            Ok(Layout {
                mechanism: Mechanism::RunKey,
                directory: PathBuf::new(),
                systemd_available: false,
            })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            Err(AutostartError::UnsupportedPlatform)
        }
    }

    /// Layout Launch Agent su una directory esplicita; solo per le prove.
    #[doc(hidden)]
    pub fn launch_agent(directory: PathBuf) -> Self {
        Layout {
            mechanism: Mechanism::LaunchAgent,
            directory,
            systemd_available: false,
        }
    }

    /// Layout systemd utente su una directory esplicita; solo per le prove.
    ///
    /// `available` sostituisce il rilevamento di `/run/systemd/system` e non
    /// avvia né interroga systemd.
    #[doc(hidden)]
    pub fn systemd_user(directory: PathBuf, available: bool) -> Self {
        Layout {
            mechanism: Mechanism::SystemdUser,
            directory,
            systemd_available: available,
        }
    }

    /// Meccanismo descritto dal layout.
    pub fn mechanism(&self) -> Mechanism {
        self.mechanism
    }

    /// Directory degli oggetti; vuota per il meccanismo Run.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Disponibilità registrata del meccanismo systemd.
    #[cfg_attr(windows, allow(dead_code))]
    pub(super) fn systemd_available(&self) -> bool {
        self.systemd_available
    }
}

#[cfg(unix)]
fn environment_path(variable: &'static str) -> Result<PathBuf, AutostartError> {
    let value =
        std::env::var_os(variable).ok_or(AutostartError::MissingEnvironment { variable })?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(AutostartError::InvalidEnvironment {
            variable,
            reason: "il percorso non è assoluto",
        });
    }
    Ok(path)
}

/// Primitive di avvio automatico per un layout verificato.
#[derive(Clone, Debug)]
pub struct Autostart {
    layout: Layout,
}

impl Autostart {
    /// Costruisce le primitive sul layout indicato.
    pub fn new(layout: Layout) -> Self {
        Self { layout }
    }

    /// Costruisce le primitive sul layout dell'account corrente.
    pub fn current() -> Result<Self, AutostartError> {
        Ok(Self::new(Layout::for_current_user()?))
    }

    /// Meccanismo usato da queste primitive.
    pub fn mechanism(&self) -> Mechanism {
        self.layout.mechanism
    }

    /// Stato della registrazione, senza modificare alcun oggetto.
    pub fn status(&self) -> Result<Status, AutostartError> {
        dispatch_status(&self.layout)
    }

    /// Registra il percorso assoluto del binario, senza argomenti.
    ///
    /// L'operazione è idempotente: un oggetto già identico resta invariato. Un
    /// oggetto proprio con un percorso diverso viene sostituito atomicamente;
    /// un oggetto estraneo o non sicuro produce un errore e resta intatto.
    pub fn install(&self, binary: &Path) -> Result<InstallReport, InstallFailure> {
        dispatch_install(&self.layout, binary)
    }

    /// Rimuove soltanto la registrazione riconosciuta come propria.
    ///
    /// Un secondo tentativo è un successo senza modifiche. Un oggetto estraneo,
    /// un collegamento inatteso o una protezione errata non viene rimosso.
    pub fn remove(&self) -> Result<RemoveReport, RemoveFailure> {
        dispatch_remove(&self.layout)
    }
}

/// Stato della registrazione, in sola lettura.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Status {
    /// Registrazione completa e riconosciuta, con il programma registrato.
    Registered { program: PathBuf },
    /// Nessun oggetto di Totum è presente.
    Absent,
    /// Oggetti presenti ma non gestibili come registrazione di Totum.
    Incompatible { detail: String },
    /// Il meccanismo non è disponibile in questa sessione.
    Unavailable { detail: String },
}

/// Esito di un singolo passo di registrazione.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallChange {
    /// Il passo non è stato eseguito, per un errore precedente.
    NotAttempted,
    /// L'oggetto era già identico.
    Unchanged,
    /// L'oggetto mancava ed è stato creato.
    Created,
    /// Un oggetto proprio con contenuto diverso è stato sostituito.
    Updated,
}

/// Esito di un singolo passo di rimozione.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoveChange {
    /// Il passo non è stato eseguito, per un errore precedente.
    NotAttempted,
    /// L'oggetto mancava già.
    AlreadyAbsent,
    /// L'oggetto proprio è stato rimosso.
    Removed,
}

/// Esito della sincronizzazione delle voci di directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Durability {
    /// Nessuna scrittura è stata completata o la durabilità non è applicabile.
    Unknown,
    /// Le voci di directory sono state sincronizzate.
    Confirmed,
    /// Le scritture sono state pubblicate, ma la durabilità non è confermata.
    NotConfirmed { detail: String },
}

/// Esito completo di una registrazione riuscita.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallReport {
    /// Percorso del binario richiesto.
    pub program: PathBuf,
    /// Esito dell'oggetto principale (plist, unità o valore).
    pub entry: InstallChange,
    /// Esito del collegamento systemd; `None` per gli altri meccanismi.
    pub link: Option<InstallChange>,
    /// Esito della sincronizzazione.
    pub durability: Durability,
}

impl InstallReport {
    fn not_attempted(program: &Path) -> Self {
        Self {
            program: program.to_path_buf(),
            entry: InstallChange::NotAttempted,
            link: None,
            durability: Durability::Unknown,
        }
    }
}

/// Errore di registrazione con il risultato parziale già ottenuto.
#[derive(Debug)]
pub struct InstallFailure {
    /// Passi completati prima dell'errore.
    pub report: InstallReport,
    /// Causa dell'errore.
    pub error: Box<AutostartError>,
}

impl fmt::Display for InstallFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl Error for InstallFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.error.as_ref())
    }
}

/// Esito completo di una rimozione riuscita.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoveReport {
    /// Esito dell'oggetto principale.
    pub entry: RemoveChange,
    /// Esito del collegamento systemd; `None` per gli altri meccanismi.
    pub link: Option<RemoveChange>,
    /// Esito della sincronizzazione.
    pub durability: Durability,
}

impl RemoveReport {
    fn not_attempted() -> Self {
        Self {
            entry: RemoveChange::NotAttempted,
            link: None,
            durability: Durability::Unknown,
        }
    }
}

/// Errore di rimozione con il risultato parziale già ottenuto.
#[derive(Debug)]
pub struct RemoveFailure {
    /// Passi completati prima dell'errore.
    pub report: RemoveReport,
    /// Causa dell'errore.
    pub error: Box<AutostartError>,
}

impl fmt::Display for RemoveFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl Error for RemoveFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.error.as_ref())
    }
}

/// Errore delle primitive di avvio automatico.
#[derive(Debug)]
pub enum AutostartError {
    /// La piattaforma non ha un meccanismo supportato.
    UnsupportedPlatform,
    /// Il processo gira con privilegi elevati.
    ElevatedExecution,
    /// Una variabile d'ambiente necessaria è assente.
    MissingEnvironment { variable: &'static str },
    /// Una variabile d'ambiente necessaria non è valida.
    InvalidEnvironment {
        variable: &'static str,
        reason: &'static str,
    },
    /// Il percorso del binario non è utilizzabile.
    InvalidProgram { path: PathBuf, reason: String },
    /// Il percorso non può essere rappresentato in sicurezza nel formato.
    NotRepresentable {
        path: PathBuf,
        mechanism: Mechanism,
        detail: String,
    },
    /// Un oggetto esistente ha tipo o protezioni non gestibili.
    UnsafeObject { path: PathBuf, detail: String },
    /// Un oggetto esistente appartiene a un'altra registrazione.
    IncompatibleObject { path: PathBuf, detail: String },
    /// Un oggetto è comparso durante la registrazione e non è stato toccato.
    ChangedDuringRegistration { path: PathBuf },
    /// Il meccanismo non è disponibile.
    Unavailable { detail: String },
    /// Errore della piattaforma.
    Platform(PlatformError),
    /// Errore di lettura o scrittura non coperto da `platform`.
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for AutostartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AutostartError::UnsupportedPlatform => {
                write!(
                    f,
                    "piattaforma senza meccanismo di avvio automatico supportato"
                )
            }
            AutostartError::ElevatedExecution => write!(
                f,
                "esecuzione elevata rifiutata: l'avvio automatico è per account e non usa privilegi amministrativi"
            ),
            AutostartError::MissingEnvironment { variable } => {
                write!(f, "variabile d'ambiente {variable} assente")
            }
            AutostartError::InvalidEnvironment { variable, reason } => {
                write!(f, "variabile d'ambiente {variable} non valida: {reason}")
            }
            AutostartError::InvalidProgram { path, reason } => {
                write!(
                    f,
                    "percorso del binario non valido {}: {reason}",
                    path.display()
                )
            }
            AutostartError::NotRepresentable {
                path,
                mechanism,
                detail,
            } => write!(
                f,
                "percorso non rappresentabile per {}: {} ({detail})",
                mechanism.name(),
                path.display()
            ),
            AutostartError::UnsafeObject { path, detail } => {
                write!(f, "oggetto non sicuro {}: {detail}", path.display())
            }
            AutostartError::IncompatibleObject { path, detail } => {
                write!(f, "oggetto incompatibile {}: {detail}", path.display())
            }
            AutostartError::ChangedDuringRegistration { path } => write!(
                f,
                "oggetto comparso durante la registrazione e lasciato intatto: {}",
                path.display()
            ),
            AutostartError::Unavailable { detail } => {
                write!(f, "meccanismo non disponibile: {detail}")
            }
            AutostartError::Platform(error) => write!(f, "{error}"),
            AutostartError::Io { path, source } => {
                write!(f, "errore di I/O su {}: {source}", path.display())
            }
        }
    }
}

impl Error for AutostartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            AutostartError::Platform(error) => Some(error),
            AutostartError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<PlatformError> for AutostartError {
    fn from(error: PlatformError) -> Self {
        match error {
            PlatformError::UnsupportedPlatform => AutostartError::UnsupportedPlatform,
            PlatformError::ElevatedExecution => AutostartError::ElevatedExecution,
            other => AutostartError::Platform(other),
        }
    }
}

/// Registra il binario con il meccanismo dell'account corrente.
pub fn install(binary: &Path) -> Result<InstallReport, InstallFailure> {
    match Autostart::current() {
        Ok(autostart) => autostart.install(binary),
        Err(error) => Err(InstallFailure {
            report: InstallReport::not_attempted(binary),
            error: Box::new(error),
        }),
    }
}

/// Legge lo stato con il meccanismo dell'account corrente.
pub fn status() -> Result<Status, AutostartError> {
    Autostart::current()?.status()
}

/// Rimuove la registrazione con il meccanismo dell'account corrente.
pub fn remove() -> Result<RemoveReport, RemoveFailure> {
    match Autostart::current() {
        Ok(autostart) => autostart.remove(),
        Err(error) => Err(RemoveFailure {
            report: RemoveReport::not_attempted(),
            error: Box::new(error),
        }),
    }
}

/// Testo del percorso del binario, validato per tutti i formati.
///
/// Richiede un percorso assoluto, non radice, senza `.` o `..`, senza
/// separatore finale, codificato UTF-8 e privo di caratteri di controllo. La
/// virgoletta doppia è ammessa dove il formato la sa codificare e viene
/// rifiutata dal solo codificatore Windows, che la usa come sintassi.
#[cfg(unix)]
pub(super) fn program_text(path: &Path, mechanism: Mechanism) -> Result<String, AutostartError> {
    let text = path
        .to_str()
        .ok_or_else(|| AutostartError::NotRepresentable {
            path: path.to_path_buf(),
            mechanism,
            detail: "il percorso non è UTF-8".to_owned(),
        })?;

    if !path.is_absolute() {
        return Err(AutostartError::InvalidProgram {
            path: path.to_path_buf(),
            reason: "il percorso deve essere assoluto".to_owned(),
        });
    }
    if text.is_empty() || text.ends_with('/') || text.ends_with('\\') {
        return Err(AutostartError::InvalidProgram {
            path: path.to_path_buf(),
            reason: "il percorso è vuoto o termina con un separatore".to_owned(),
        });
    }
    if text
        .split(['/', '\\'])
        .any(|part| part == "." || part == "..")
    {
        return Err(AutostartError::InvalidProgram {
            path: path.to_path_buf(),
            reason: "il percorso non può contenere '.' o '..'".to_owned(),
        });
    }
    if text.chars().any(|c| c.is_control()) {
        return Err(AutostartError::NotRepresentable {
            path: path.to_path_buf(),
            mechanism,
            detail: "il percorso contiene caratteri di controllo".to_owned(),
        });
    }
    if path.components().all(|component| {
        matches!(
            component,
            std::path::Component::RootDir | std::path::Component::Prefix(_)
        )
    }) {
        return Err(AutostartError::InvalidProgram {
            path: path.to_path_buf(),
            reason: "il percorso non può essere la radice".to_owned(),
        });
    }

    Ok(text.to_owned())
}

/// Converte un errore di protezione in una descrizione per lo stato.
///
/// Gli errori di classificazione descrivono un oggetto incompatibile, non un
/// guasto di I/O: lo stato li riporta invece di interrompere la lettura.
#[cfg(unix)]
pub(super) fn protection_detail(error: &PlatformError) -> Option<String> {
    match error {
        PlatformError::SymlinkRefused { .. }
        | PlatformError::ReparsePointRefused { .. }
        | PlatformError::NotADirectory { .. }
        | PlatformError::NotARegularFile { .. }
        | PlatformError::NotASymlink { .. }
        | PlatformError::WrongOwner { .. }
        | PlatformError::WrongMode { .. }
        | PlatformError::AclViolation { .. }
        | PlatformError::UnverifiableProtection { .. }
        | PlatformError::PathChanged { .. }
        | PlatformError::InvalidPath { .. } => Some(error.to_string()),
        _ => None,
    }
}

#[cfg(unix)]
fn dispatch_status(layout: &Layout) -> Result<Status, AutostartError> {
    unix::status(layout)
}

#[cfg(unix)]
fn dispatch_install(layout: &Layout, binary: &Path) -> Result<InstallReport, InstallFailure> {
    unix::install(layout, binary)
}

#[cfg(unix)]
fn dispatch_remove(layout: &Layout) -> Result<RemoveReport, RemoveFailure> {
    unix::remove(layout)
}

#[cfg(windows)]
fn dispatch_status(layout: &Layout) -> Result<Status, AutostartError> {
    windows::status(layout)
}

#[cfg(windows)]
fn dispatch_install(layout: &Layout, binary: &Path) -> Result<InstallReport, InstallFailure> {
    windows::install(layout, binary)
}

#[cfg(windows)]
fn dispatch_remove(layout: &Layout) -> Result<RemoveReport, RemoveFailure> {
    windows::remove(layout)
}

#[cfg(not(any(unix, windows)))]
fn dispatch_status(_layout: &Layout) -> Result<Status, AutostartError> {
    Err(AutostartError::UnsupportedPlatform)
}

#[cfg(not(any(unix, windows)))]
fn dispatch_install(_layout: &Layout, binary: &Path) -> Result<InstallReport, InstallFailure> {
    Err(InstallFailure {
        report: InstallReport::not_attempted(binary),
        error: Box::new(AutostartError::UnsupportedPlatform),
    })
}

#[cfg(not(any(unix, windows)))]
fn dispatch_remove(_layout: &Layout) -> Result<RemoveReport, RemoveFailure> {
    Err(RemoveFailure {
        report: RemoveReport::not_attempted(),
        error: Box::new(AutostartError::UnsupportedPlatform),
    })
}
