//! Piattaforma: radice di stato per account, permessi effettivi e ancoraggio.
//!
//! Il modulo implementa i passi P1/P2 e P5 di D3 come libreria testabile:
//! risolve il percorso di stato dell'account, rileva l'esecuzione elevata,
//! verifica antenati e radice, crea i componenti mancanti con protezioni imposte
//! e verificate, rifiuta collegamenti simbolici e reparse point, ancora le
//! operazioni alla directory verificata, offre primitive minime per creare e
//! aprire file e il lock esclusivo di istanza unica su `state.lock`.
//!
//! Non è collegato all'avvio dei binari: nessun percorso operativo lo usa
//! finché un incremento successivo non imporrà l'ordine di inizializzazione.
//!
//! Regole comuni:
//!
//! - una protezione non verificabile è un errore esplicito, mai un ripiego;
//! - un oggetto preesistente che non corrisponde alle attese viene rifiutato e
//!   non riparato;
//! - nessuna funzione esegue comandi shell o processi esterni.

mod error;

pub use error::PlatformError;

use std::ffi::OsStr;
use std::fs::{File, TryLockError};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
mod linux_acl;
#[cfg(target_os = "macos")]
mod macos_acl;

// Il parser è puro: esiste anche nelle build di test su macOS, dove gli ACL
// Linux non sono altrimenti raggiungibili.
#[cfg(any(target_os = "linux", all(test, unix)))]
mod posix_acl;

#[cfg(all(test, unix))]
mod tests;

/// Politica di lettura degli ACL applicata a un oggetto.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum AclPolicy {
    /// Nessun accesso ad altri: ogni voce di tipo allow per un altro utente è
    /// una violazione. Usata per la radice e per i file.
    OwnerOnly,
    /// Solo il diritto di scrittura, che permette di aggiungere, cancellare o
    /// sostituire voci nella directory. Lettura ed esecuzione, che consentono
    /// rispettivamente di elencare e attraversare, non sono violazioni. Usata
    /// per gli antenati.
    DirectoryReplacement,
}

/// Esito della sincronizzazione della voce di directory della radice.
///
/// Su Unix la voce viene sincronizzata e un errore è restituito al chiamante;
/// su Windows la libreria standard e `windows-sys` non offrono un equivalente
/// generale e l'esito è sempre [`DirectorySync::Unsupported`]: la durabilità
/// della voce di directory non è dichiarata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectorySync {
    /// La voce di directory è stata sincronizzata.
    Synced,
    /// La piattaforma non offre la sincronizzazione della voce di directory.
    Unsupported,
}

/// Identificativo effettivo dell'utente sui sistemi Unix.
#[cfg(unix)]
pub(crate) fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[cfg(unix)]
pub use unix::VerifiedRoot;
#[cfg(unix)]
pub(crate) use unix::{open_containing_directory, prepare_containing_directory};
#[cfg(windows)]
pub use windows::VerifiedRoot;
#[cfg(windows)]
pub(crate) use windows::{RegistryCreationSecurity, current_user_sid, registry_security};

/// Tipo di voce osservato senza seguire collegamenti.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EntryKind {
    Regular,
    Directory,
    Symlink,
    Other,
}

/// Metadati minimi di una voce, letti senza seguirla.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct EntryInfo {
    pub kind: EntryKind,
    pub mode: u32,
    pub uid: u32,
}

/// Tipo di voce di una DACL di registro Windows.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RegistryAceKind {
    /// Accesso consentito.
    Allowed,
    /// Accesso negato.
    Denied,
    /// Controllo.
    Audit,
    /// Allarme.
    Alarm,
    /// Tipo non interpretabile dalla politica.
    Other,
}

/// Voce di una DACL di registro Windows, senza dipendenze dalle API Win32.
#[cfg(any(windows, test))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RegistryAce {
    /// Tipo della voce.
    pub kind: RegistryAceKind,
    /// La voce vale soltanto per gli oggetti ereditati.
    pub inherit_only: bool,
    /// Maschera dei diritti.
    pub mask: u32,
    /// SID del titolare, assente se non convertibile.
    pub trustee: Option<String>,
}

/// Protezioni effettive di una chiave di registro, lette dall'handle in uso.
#[cfg(any(windows, test))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RegistrySecurity {
    /// SID del proprietario, assente se non convertibile.
    pub owner: Option<String>,
    /// Indica se la chiave ha una DACL; una DACL assente concede tutto.
    pub dacl_present: bool,
    /// Indica se la DACL è protetta dall'ereditarietà.
    pub protected: bool,
    /// Voci della DACL effettiva.
    pub entries: Vec<RegistryAce>,
}

#[cfg(not(any(unix, windows)))]
pub type VerifiedRoot = std::convert::Infallible;

/// Radice di stato dell'account corrente, in forma nominale e non verificata.
///
/// La radice deriva soltanto dalle variabili di profilo del sistema: nessuna
/// variabile dedicata a Totum la sposta. Il percorso restituito è assoluto;
/// la verifica di antenati, proprietà e protezioni avviene in [`prepare_root`].
///
/// - macOS: `$HOME/Library/Application Support/Totum`
/// - Linux: `${XDG_STATE_HOME:-$HOME/.local/state}/totum`
/// - Windows: `%LOCALAPPDATA%\Totum`
pub fn state_root() -> Result<PathBuf, PlatformError> {
    #[cfg(unix)]
    {
        unix::state_root()
    }
    #[cfg(windows)]
    {
        windows::state_root()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(PlatformError::UnsupportedPlatform)
    }
}

/// Indica se il processo gira con privilegi elevati.
///
/// Su macOS e Linux confronta l'identificativo effettivo con 0; su Windows legge
/// il token elevato del processo, non l'appartenenza al gruppo amministrativo.
/// Un errore di lettura è un errore esplicito, mai un valore predefinito.
pub fn is_elevated() -> Result<bool, PlatformError> {
    #[cfg(unix)]
    {
        unix::is_elevated()
    }
    #[cfg(windows)]
    {
        windows::is_elevated()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(PlatformError::UnsupportedPlatform)
    }
}

/// Verifica o crea la radice di stato e la ancora a un descrittore.
///
/// Ordine: validazione del percorso nominale, verifica in sola lettura degli
/// antenati esistenti, rifiuto dell'esecuzione elevata, creazione protetta dei
/// soli componenti mancanti, verifica della radice, ancoraggio.
///
/// Un oggetto preesistente non sicuro produce un errore e non viene riparato.
pub fn prepare_root(nominal: &Path) -> Result<VerifiedRoot, PlatformError> {
    #[cfg(unix)]
    {
        unix::prepare_root(nominal)
    }
    #[cfg(windows)]
    {
        windows::prepare_root(nominal)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = nominal;
        Err(PlatformError::UnsupportedPlatform)
    }
}

/// Sincronizza i dati e i metadati di un file già scritto.
///
/// Su macOS usa `fcntl(F_FULLFSYNC)`, l'unica primitiva che la documentazione
/// Apple indica come sufficiente per la persistenza su disco; su Linux `fsync`;
/// su Windows `FlushFileBuffers` tramite `File::sync_all`. Un errore è
/// esplicito: nessuna scrittura viene dichiarata sincronizzata per omissione.
pub fn sync_file(file: &std::fs::File, path: &std::path::Path) -> Result<(), PlatformError> {
    #[cfg(unix)]
    {
        unix::sync_file(file, path)
    }
    #[cfg(windows)]
    {
        windows::sync_file(file, path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, path);
        Err(PlatformError::UnsupportedPlatform)
    }
}

/// Nome del file di lock di istanza unica nella radice di stato.
pub const LOCK_FILE: &str = "state.lock";

/// Radice di stato verificata e ancorata, con il proprio lock esclusivo.
///
/// Si ottiene soltanto con [`lock`], che consuma la radice e acquisisce il lock
/// sulla **stessa** directory ancorata: non esiste un valore di lock separato
/// che possa essere passato a una radice diversa. L'associazione è quella
/// dell'identità della directory (descrittore e inode su Unix, handle e
/// identificativo di volume e file su Windows), non del percorso nominale: una
/// rinomina o la sostituzione della directory con un'altra che occupa lo stesso
/// percorso non cambia la directory protetta dal guard.
///
/// Il lock è consultivo, non obbligatorio: coordina le istanze di Totum che
/// rispettano il protocollo, non software dello stesso account che lo ignora.
/// È detenuto dal sistema operativo finché l'oggetto è vivo e viene rilasciato
/// alla sua chiusura o alla morte del processo; la chiusura **non** rimuove né
/// sostituisce `state.lock`, e l'occupazione non dipende mai dal contenuto del
/// file o da un PID.
///
/// Va acquisito con [`lock`] prima della classificazione operativa e conservato
/// per tutta la vita di chi scrive lo stato: [`crate::config::Store`] lo
/// richiede per essere costruito.
#[must_use = "il lock resta esclusivo finché la radice bloccata è viva: scartarla lo rilascia"]
#[derive(Debug)]
pub struct LockedRoot {
    root: VerifiedRoot,
    // Il descrittore è la sorgente del lock: chiudendolo il sistema lo rilascia.
    // Vive insieme alla radice che ha prodotto l'acquisizione.
    _file: File,
    lock_path: PathBuf,
    consumers: Mutex<Consumers>,
}

/// Componente che trattiene un accesso esclusivo alla radice.
///
/// Configurazione e journal possono coesistere perché occupano due slot
/// distinti; due istanze dello stesso componente no, perché ognuna avrebbe la
/// propria serializzazione interna e non vedrebbe le scritture dell'altra.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Consumer {
    Config,
    Journal,
}

#[derive(Debug, Default)]
struct Consumers {
    config: bool,
    journal: bool,
}

impl Consumers {
    fn slot(&mut self, consumer: Consumer) -> &mut bool {
        match consumer {
            Consumer::Config => &mut self.config,
            Consumer::Journal => &mut self.journal,
        }
    }
}

/// Occupazione di uno slot della radice, rilasciata al `Drop`.
///
/// Trattiene un clone di [`SharedRoot`], quindi il lock resta acquisito finché
/// l'occupante vive, anche se chi l'ha aperto non conserva altri accessi.
#[derive(Debug)]
pub(crate) struct RootClaim {
    root: SharedRoot,
    consumer: Consumer,
}

impl RootClaim {
    /// Radice ancorata mentre l'occupazione è valida.
    pub(crate) fn verified_root(&self) -> &VerifiedRoot {
        self.root.verified_root()
    }
}

impl Drop for RootClaim {
    fn drop(&mut self) {
        let mut consumers = self
            .root
            .0
            .consumers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *consumers.slot(self.consumer) = false;
    }
}

impl LockedRoot {
    /// Percorso della radice di stato ancorata.
    pub fn path(&self) -> &Path {
        self.root.path()
    }

    /// Percorso di `state.lock` nella radice ancorata, per messaggi e prove.
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    pub(crate) fn verified_root(&self) -> &VerifiedRoot {
        &self.root
    }

    /// Trasforma l'oggetto unico in un accesso condivisibile.
    ///
    /// Consuma [`LockedRoot`]: da questo punto il lock è trattenuto da
    /// [`SharedRoot`] e resta acquisito finché esiste almeno un accesso. Dopo
    /// questa chiamata non è più possibile ottenere il riferimento esclusivo
    /// richiesto da [`crate::identity::initialize`](crate::identity), quindi
    /// l'inizializzazione dell'identità resta esclusiva per costruzione.
    pub fn share(self) -> SharedRoot {
        SharedRoot(Arc::new(self))
    }
}

/// Accesso condiviso alla radice bloccata, con il lock trattenuto.
///
/// Si ottiene soltanto da [`LockedRoot::share`]. Configurazione e journal ne
/// ricevono cloni distinti: ogni utilizzatore trattiene il lock finché vive e
/// il sistema lo rilascia soltanto quando l'ultimo clone viene chiuso. Un
/// accesso non può essere associato a un'altra radice: l'identità è quella
/// della directory ancorata, non del percorso nominale.
///
/// Lo stesso accesso registra anche chi lo sta usando: al massimo uno [`Store`]
/// della configurazione e un journal possono essere aperti sulla stessa radice,
/// mentre due istanze dello stesso componente vengono rifiutate prima di
/// leggere o toccare i file. Senza questa esclusione ognuna avrebbe il proprio
/// mutex e non serializzerebbe le scritture dell'altra.
///
/// [`Store`]: crate::config::Store
#[derive(Clone, Debug)]
pub struct SharedRoot(Arc<LockedRoot>);

impl SharedRoot {
    /// Percorso della radice di stato ancorata.
    pub fn path(&self) -> &Path {
        self.0.path()
    }

    /// Radice verificata e ancorata sottostante.
    pub(crate) fn verified_root(&self) -> &VerifiedRoot {
        self.0.verified_root()
    }

    /// Occupa lo slot del componente, o restituisce [`None`] se è già occupato.
    ///
    /// Il controllo avviene prima di qualunque accesso ai file di stato: una
    /// seconda apertura dello stesso componente non arriva al motore del
    /// journal né alla classificazione della configurazione.
    pub(crate) fn claim(&self, consumer: Consumer) -> Option<RootClaim> {
        let mut consumers = self
            .0
            .consumers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let slot = consumers.slot(consumer);
        if *slot {
            return None;
        }
        *slot = true;
        Some(RootClaim {
            root: self.clone(),
            consumer,
        })
    }
}

impl From<LockedRoot> for SharedRoot {
    fn from(root: LockedRoot) -> Self {
        root.share()
    }
}

/// Acquisisce il lock esclusivo della radice, senza attendere.
///
/// Consuma la radice e restituisce [`LockedRoot`], che possiede insieme la
/// radice ancorata e il lock: non è possibile associare il lock a un'altra
/// radice, nemmeno con lo stesso percorso nominale. `state.lock` viene creato
/// se manca, con modalità 0600 o DACL equivalente imposta e verificata, oppure
/// aperto se esiste: tipo regolare, proprietario, protezioni e assenza di
/// collegamenti o reparse point sono verificati e un oggetto non sicuro è
/// rifiutato senza riparazione. L'acquisizione non attende mai: se un'altra
/// istanza detiene il lock, l'esito è [`PlatformError::AlreadyRunning`] e la
/// radice deve essere preparata di nuovo per un altro tentativo. Il file non
/// viene letto né scritto.
pub fn lock(root: VerifiedRoot) -> Result<LockedRoot, PlatformError> {
    #[cfg(any(unix, windows))]
    {
        let path = root.path().join(LOCK_FILE);
        let file = root.open_lock_file(OsStr::new(LOCK_FILE))?;
        match file.try_lock() {
            Ok(()) => Ok(LockedRoot {
                root,
                _file: file,
                lock_path: path,
                consumers: Mutex::new(Consumers::default()),
            }),
            Err(TryLockError::WouldBlock) => Err(PlatformError::AlreadyRunning { path }),
            Err(TryLockError::Error(source)) => Err(PlatformError::io(path, source)),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = root;
        Err(PlatformError::UnsupportedPlatform)
    }
}

/// Valida il percorso nominale: assoluto, senza `..` e senza finire sulla radice.
pub(crate) fn validate_nominal(path: &Path) -> Result<(), PlatformError> {
    if !path.is_absolute() {
        return Err(PlatformError::invalid(
            path,
            "il percorso deve essere assoluto",
        ));
    }
    // `Path::components` normalizza il componente corrente: il controllo va
    // fatto sui byte originali per non ammettere "." per omissione.
    let text = path.as_os_str().as_encoded_bytes();
    if text
        .split(|byte| *byte == b'/' || *byte == b'\\')
        .any(|part| part == b"." || part == b"..")
    {
        return Err(PlatformError::invalid(
            path,
            "il percorso non può contenere '.' o '..'",
        ));
    }
    let mut only_root = true;
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(PlatformError::invalid(
                    path,
                    "il percorso non può contenere '..'",
                ));
            }
            Component::CurDir => {
                return Err(PlatformError::invalid(
                    path,
                    "il percorso non può contenere '.'",
                ));
            }
            Component::Normal(_) => only_root = false,
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    if only_root {
        return Err(PlatformError::invalid(
            path,
            "la radice di stato non può essere la radice del filesystem",
        ));
    }
    Ok(())
}

/// Valida un nome di file: un solo componente normale, senza separatori.
pub(crate) fn validate_single_component(name: &OsStr) -> Result<&OsStr, PlatformError> {
    let path = Path::new(name);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(value)), None) if value == name => Ok(name),
        _ => Err(PlatformError::InvalidPath {
            path: PathBuf::from(name),
            reason: "il nome deve essere un singolo componente senza separatori",
        }),
    }
}
