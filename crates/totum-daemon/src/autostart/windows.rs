//! Implementazione Windows: valore dedicato sotto la chiave `Run` dell'utente.
//!
//! Il valore è `REG_SZ` e contiene soltanto il percorso assoluto del binario,
//! senza argomenti. La scrittura avviene in `HKEY_CURRENT_USER` e non richiede
//! elevazione. Prima di qualunque modifica vengono lette le protezioni
//! effettive dell'handle usato e degli antenati pertinenti: una DACL assente o
//! non leggibile, un proprietario inatteso o un diritto di modifica concesso a
//! un account diverso dall'utente, da `SYSTEM` o dagli amministratori sono un
//! rifiuto, mai una riparazione. La creazione usa una DACL protetta già
//! nell'handle nuovo, quindi non esiste una finestra iniziale non protetta.
//!
//! Le API del registro non offrono una conferma di durabilità equivalente a
//! `fsync`: l'esito resta [`Durability::Unknown`]. Nessun valore diverso da
//! quello di Totum viene modificato o rimosso.

use std::io;
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_CREATED_NEW_KEY, REG_OPTION_NON_VOLATILE,
    REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW,
};

use crate::platform::{self, PlatformError};

use super::registry::{self, Backend, Role, Snapshot, Value};
use super::{
    AutostartError, Durability, InstallChange, InstallFailure, InstallReport, Layout, Mechanism,
    RemoveChange, RemoveFailure, RemoveReport, Status, command_line,
};

const KEY_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const ANCESTORS: [&str; 4] = [
    "Software",
    "Software\\Microsoft",
    "Software\\Microsoft\\Windows",
    "Software\\Microsoft\\Windows\\CurrentVersion",
];

/// Percorso della chiave e catena degli antenati verificati.
///
/// La produzione usa soltanto [`production_location`]. Le prove costruiscono
/// un [`Location`] esplicito sotto una chiave isolata: non esiste un'opzione,
/// una variabile o un bypass del percorso reale.
#[derive(Clone, Debug)]
struct Location {
    key_path: String,
    ancestors: Vec<String>,
}

fn production_location() -> Location {
    Location {
        key_path: KEY_PATH.to_owned(),
        ancestors: ANCESTORS.iter().map(|path| (*path).to_owned()).collect(),
    }
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: l'handle è stato aperto o creato da questo modulo e non è
        // più usato dopo il guard.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

struct StoredValue {
    kind: REG_VALUE_TYPE,
    text: Option<String>,
}

/// Backend reale: legge e ricontrolla le protezioni sull'handle che usa.
struct RunKeyBackend {
    user: String,
    location: Location,
}

impl RunKeyBackend {
    fn production(user: String) -> Self {
        Self {
            user,
            location: production_location(),
        }
    }
}

impl Backend for RunKeyBackend {
    fn snapshot(&mut self) -> Result<Snapshot, AutostartError> {
        let path = registry_path(&self.location);
        let ancestors = read_ancestor_security(&self.location)?;
        let key = open_subkey(&self.location.key_path, KEY_READ)?;
        let (key_security, value) = match key {
            Some(key) => (Some(read_security(key.0, &path)?), read_value(&key, &path)?),
            None => (None, Value::Absent),
        };
        Ok(Snapshot {
            key: key_security,
            ancestors,
            value,
        })
    }

    fn write_value(&mut self, command: &str, expected: &Snapshot) -> Result<(), AutostartError> {
        let path = registry_path(&self.location);
        let (key, created) = match open_subkey(&self.location.key_path, KEY_READ | KEY_SET_VALUE)? {
            Some(key) => (key, false),
            None => {
                if expected.key.is_some() {
                    return Err(AutostartError::ChangedDuringRegistration { path });
                }
                let security = platform::RegistryCreationSecurity::new()?;
                let attributes = security.attributes();
                let (key, disposition) = create_key(
                    &self.location.key_path,
                    KEY_READ | KEY_SET_VALUE,
                    &attributes,
                )?;
                (key, disposition == REG_CREATED_NEW_KEY)
            }
        };
        if created != expected.key.is_none() {
            return Err(AutostartError::ChangedDuringRegistration { path });
        }

        verify_key_security(key.0, &self.user, &path)?;
        verify_ancestor_security(&self.location, &self.user)?;
        if read_value(&key, &path)? != expected.value {
            return Err(AutostartError::ChangedDuringRegistration { path });
        }
        set_value(&key, command, &path)?;
        Ok(())
    }

    fn delete_value(&mut self, expected: &Snapshot) -> Result<(), AutostartError> {
        let path = registry_path(&self.location);
        let Some(key) = open_subkey(&self.location.key_path, KEY_READ | KEY_SET_VALUE)? else {
            return Ok(());
        };
        verify_key_security(key.0, &self.user, &path)?;
        verify_ancestor_security(&self.location, &self.user)?;
        if read_value(&key, &path)? != expected.value {
            return Err(AutostartError::ChangedDuringRegistration { path });
        }
        delete_value(&key, &path)
    }
}

fn read_ancestor_security(
    location: &Location,
) -> Result<Vec<platform::RegistrySecurity>, AutostartError> {
    let mut ancestors = Vec::with_capacity(location.ancestors.len());
    for subkey in &location.ancestors {
        let path = display_path(subkey);
        let Some(key) = open_subkey(subkey, KEY_READ)? else {
            return Err(AutostartError::Io {
                path,
                source: io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32),
            });
        };
        ancestors.push(read_security(key.0, &path)?);
    }
    Ok(ancestors)
}

fn verify_ancestor_security(location: &Location, user: &str) -> Result<(), AutostartError> {
    for security in read_ancestor_security(location)? {
        registry::check_security(&security, Role::Ancestor, user).map_err(|error| {
            AutostartError::UnsafeObject {
                path: registry_path(location),
                detail: error.detail,
            }
        })?;
    }
    Ok(())
}

fn verify_key_security(handle: HKEY, user: &str, path: &Path) -> Result<(), AutostartError> {
    let security = read_security(handle, path)?;
    registry::check_security(&security, Role::Key, user).map_err(|error| {
        AutostartError::UnsafeObject {
            path: path.to_path_buf(),
            detail: error.detail,
        }
    })
}

pub(super) fn status(layout: &Layout) -> Result<Status, AutostartError> {
    if layout.mechanism() != Mechanism::RunKey {
        return Ok(Status::Unavailable {
            detail: "il meccanismo richiesto non è disponibile su Windows".to_owned(),
        });
    }
    let user = platform::current_user_sid()?;
    let mut backend = RunKeyBackend::production(user);
    let snapshot = backend.snapshot()?;
    Ok(registry::status(&snapshot, &backend.user))
}

pub(super) fn install(
    layout: &Layout,
    binary: &std::path::Path,
) -> Result<InstallReport, InstallFailure> {
    let mut report = InstallReport {
        program: binary.to_path_buf(),
        entry: InstallChange::NotAttempted,
        link: None,
        durability: Durability::Unknown,
    };
    let result = install_inner(layout, binary, &mut report);
    match result {
        Ok(()) => Ok(report),
        Err(error) => Err(InstallFailure {
            report,
            error: Box::new(error),
        }),
    }
}

fn install_inner(
    layout: &Layout,
    binary: &std::path::Path,
    report: &mut InstallReport,
) -> Result<(), AutostartError> {
    if layout.mechanism() != Mechanism::RunKey {
        return Err(AutostartError::Unavailable {
            detail: "il meccanismo richiesto non è disponibile su Windows".to_owned(),
        });
    }
    if platform::is_elevated()? {
        return Err(AutostartError::ElevatedExecution);
    }
    let command = command_line::encode(binary)?;
    let user = platform::current_user_sid()?;
    let location = production_location();
    let path = registry_path(&location);
    let mut backend = RunKeyBackend {
        user: user.clone(),
        location,
    };
    report.entry = registry::install(&mut backend, &command, &user, &path)?;
    Ok(())
}

pub(super) fn remove(layout: &Layout) -> Result<RemoveReport, RemoveFailure> {
    let mut report = RemoveReport {
        entry: RemoveChange::NotAttempted,
        link: None,
        durability: Durability::Unknown,
    };
    let result = remove_inner(layout, &mut report);
    match result {
        Ok(()) => Ok(report),
        Err(error) => Err(RemoveFailure {
            report,
            error: Box::new(error),
        }),
    }
}

fn remove_inner(layout: &Layout, report: &mut RemoveReport) -> Result<(), AutostartError> {
    if layout.mechanism() != Mechanism::RunKey {
        return Err(AutostartError::Unavailable {
            detail: "il meccanismo richiesto non è disponibile su Windows".to_owned(),
        });
    }
    if platform::is_elevated()? {
        return Err(AutostartError::ElevatedExecution);
    }
    let user = platform::current_user_sid()?;
    let location = production_location();
    let path = registry_path(&location);
    let mut backend = RunKeyBackend {
        user: user.clone(),
        location,
    };
    report.entry = registry::remove(&mut backend, &user, &path)?;
    Ok(())
}

fn read_security(handle: HKEY, path: &Path) -> Result<platform::RegistrySecurity, AutostartError> {
    platform::registry_security(handle, path).map_err(|error| protections_error(path, error))
}

fn protections_error(path: &Path, error: PlatformError) -> AutostartError {
    if matches!(
        &error,
        PlatformError::UnverifiableProtection { .. }
            | PlatformError::AclViolation { .. }
            | PlatformError::WrongOwner { .. }
    ) {
        AutostartError::UnsafeObject {
            path: path.to_path_buf(),
            detail: error.to_string(),
        }
    } else {
        error.into()
    }
}

fn open_subkey(subkey: &str, access: u32) -> Result<Option<Key>, AutostartError> {
    let text = wide(subkey);
    let mut key: HKEY = ptr::null_mut();
    // SAFETY: il percorso è terminato da NUL e l'handle di destinazione è
    // valido per la durata della chiamata.
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, text.as_ptr(), 0, access, &mut key) };
    match status {
        ERROR_SUCCESS => Ok(Some(Key(key))),
        ERROR_FILE_NOT_FOUND => Ok(None),
        other => Err(registry_error(&display_path(subkey), other)),
    }
}

fn create_key(
    key_path: &str,
    access: u32,
    attributes: &windows_sys::Win32::Security::SECURITY_ATTRIBUTES,
) -> Result<(Key, u32), AutostartError> {
    let subkey = wide(key_path);
    let mut key: HKEY = ptr::null_mut();
    let mut disposition = 0u32;
    // SAFETY: il descrittore è valido per la durata della chiamata e i
    // puntatori di output sono validi.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            access,
            attributes,
            &mut key,
            &mut disposition,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(registry_error(&display_path(key_path), status));
    }
    Ok((Key(key), disposition))
}

fn read_value(key: &Key, path: &Path) -> Result<Value, AutostartError> {
    match query_value(key, path)? {
        None => Ok(Value::Absent),
        Some(stored) => Ok(Value::Present {
            kind: stored.kind,
            text: stored.text,
        }),
    }
}

fn query_value(key: &Key, path: &Path) -> Result<Option<StoredValue>, AutostartError> {
    let name = wide(command_line::VALUE_NAME);
    let mut kind: REG_VALUE_TYPE = 0;
    let mut size = 0u32;
    // SAFETY: la dimensione viene scritta dall'API; nessun buffer è passato.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            ptr::null(),
            &mut kind,
            ptr::null_mut(),
            &mut size,
        )
    };
    match status {
        ERROR_SUCCESS => {}
        ERROR_FILE_NOT_FOUND => return Ok(None),
        other => return Err(registry_error(path, other)),
    }
    if kind != REG_SZ {
        return Ok(Some(StoredValue { kind, text: None }));
    }

    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut buffer = vec![0u16; (size as usize).div_ceil(2).max(1)];
        let mut used = size;
        // SAFETY: il buffer ha la capacità dichiarata da `size`; l'API
        // scrive al massimo `used` byte e aggiorna `used`.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                ptr::null(),
                &mut kind,
                buffer.as_mut_ptr().cast::<u8>(),
                &mut used,
            )
        };
        match status {
            ERROR_SUCCESS => {
                if kind != REG_SZ {
                    return Ok(Some(StoredValue { kind, text: None }));
                }
                let units = (used as usize) / 2;
                let mut text = String::from_utf16(&buffer[..units.min(buffer.len())]).ok();
                if let Some(value) = &mut text {
                    while value.ends_with('\0') {
                        value.pop();
                    }
                }
                return Ok(Some(StoredValue { kind, text }));
            }
            ERROR_MORE_DATA if attempt < 3 => {
                size = used;
            }
            other => return Err(registry_error(path, other)),
        }
    }
}

fn set_value(key: &Key, command: &str, path: &Path) -> Result<(), AutostartError> {
    let name = wide(command_line::VALUE_NAME);
    let data: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = data
        .len()
        .checked_mul(2)
        .and_then(|size| u32::try_from(size).ok())
        .ok_or_else(|| AutostartError::NotRepresentable {
            path: path.to_path_buf(),
            mechanism: Mechanism::RunKey,
            detail: "valore Run troppo grande".to_owned(),
        })?;
    // SAFETY: il buffer è valido e la lunghezza è quella dichiarata.
    let status = unsafe {
        RegSetValueExW(
            key.0,
            name.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr().cast::<u8>(),
            bytes,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(registry_error(path, status));
    }
    Ok(())
}

fn delete_value(key: &Key, path: &Path) -> Result<(), AutostartError> {
    let name = wide(command_line::VALUE_NAME);
    // SAFETY: l'handle e il nome sono validi per la durata della chiamata.
    let status = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
    if status != ERROR_SUCCESS {
        return Err(registry_error(path, status));
    }
    Ok(())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn display_path(subkey: &str) -> PathBuf {
    PathBuf::from(format!("HKCU\\{subkey}"))
}

fn registry_path(location: &Location) -> PathBuf {
    display_path(&format!(
        "{}\\{}",
        location.key_path,
        command_line::VALUE_NAME
    ))
}

fn registry_error(path: &Path, status: u32) -> AutostartError {
    AutostartError::Io {
        path: path.to_path_buf(),
        source: io::Error::from_raw_os_error(status as i32),
    }
}

/// Prove del backend reale del registro su una chiave isolata (D3 P10-A).
///
/// Le prove aprono e modificano soltanto
/// `HKCU\Software\Homun\Totum\P10ATests\<pid>-<contatore>` e i suoi antenati,
/// mai `Software\Microsoft\Windows\CurrentVersion\Run`. La chiave isolata viene
/// creata con il descrittore di sicurezza di produzione, cancellata con
/// `RegDeleteTreeW` anche se una prova fallisce, e il valore reale della chiave
/// `Run` viene confrontato prima e dopo. Il percorso isolato è costruito qui:
/// non esiste un'opzione o una variabile che possa spostare il backend di
/// produzione.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSidToSidW, SE_REGISTRY_KEY, SetSecurityInfo,
    };
    use windows_sys::Win32::Security::{
        ACL, ACL_REVISION, AddAccessAllowedAce, DACL_SECURITY_INFORMATION, InitializeAcl,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID,
    };
    use windows_sys::Win32::System::Registry::RegDeleteTreeW;

    const KEY_ALL_ACCESS: u32 = 0x000F_003F;
    const WRITE_DAC: u32 = 0x0004_0000;
    const SYSTEM_SID: &str = "S-1-5-18";
    const ADMINISTRATORS_SID: &str = "S-1-5-32-544";
    const USERS_SID: &str = "S-1-5-32-545";

    /// Percorso di prova univoco e i suoi antenati verificati.
    ///
    /// L'antenato `Software` esiste e appartiene all'utente; gli altri vengono
    /// creati con il descrittore di sicurezza di produzione.
    /// [`Isolated::create_key_only`] lascia **assente** la chiave della prova,
    /// che deve creare il backend; [`Isolated::create_key_and_value`] la crea
    /// per il caso che richiede una chiave preesistente con protezioni alterate.
    /// La sola directory univoca viene rimossa alla fine, lasciando intatti i
    /// contenitori condivisi come fa il modulo con le directory.
    static NEXT: AtomicU32 = AtomicU32::new(0);

    struct Isolated {
        root: String,
        key_path: String,
        ancestors: Vec<String>,
    }

    impl Isolated {
        /// Crea la sottochiave univoca e i suoi antenati, lasciando **assente**
        /// la chiave della prova: deve essere il backend a crearla.
        fn create_key_only() -> Self {
            let isolated = Self::new();
            assert!(
                open_subkey(&isolated.key_path, KEY_READ)
                    .expect("lettura dello stato della chiave")
                    .is_none(),
                "la chiave della prova deve mancare prima del backend: {}",
                isolated.key_path
            );
            isolated
        }

        /// Come [`Self::create_key_only`] ma crea anche la chiave, per il caso
        /// che richiede una chiave preesistente con protezioni alterate.
        fn create_key_and_value() -> Self {
            let isolated = Self::create_key_only();
            create_protected(&isolated.key_path);
            isolated
        }

        fn new() -> Self {
            let unique = format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let root = format!("Software\\Homun\\Totum\\P10ATests\\{unique}");
            let key_path = format!("{root}\\Run");
            let ancestors = vec![
                "Software".to_owned(),
                "Software\\Homun".to_owned(),
                "Software\\Homun\\Totum".to_owned(),
                "Software\\Homun\\Totum\\P10ATests".to_owned(),
                root.clone(),
            ];
            for path in &ancestors {
                create_protected(path);
            }
            Self {
                root,
                key_path,
                ancestors,
            }
        }

        fn location(&self) -> Location {
            Location {
                key_path: self.key_path.clone(),
                ancestors: self.ancestors.clone(),
            }
        }

        fn value_path(&self) -> PathBuf {
            display_path(&format!("{}\\{}", self.key_path, command_line::VALUE_NAME))
        }
    }

    impl Drop for Isolated {
        fn drop(&mut self) {
            let root = wide(&self.root);
            // SAFETY: il percorso è valido e l'albero è quello creato dalla
            // prova; l'errore di pulizia non maschera l'esito della prova.
            unsafe {
                RegDeleteTreeW(HKEY_CURRENT_USER, root.as_ptr());
            }
        }
    }

    fn create_protected(path: &str) {
        let security = platform::RegistryCreationSecurity::new().expect("descrittore protetto");
        let attributes = security.attributes();
        let (key, _) = create_key(path, KEY_READ | KEY_SET_VALUE, &attributes)
            .unwrap_or_else(|error| panic!("chiave {path} creabile: {error:?}"));
        drop(key);
    }

    /// Protezioni effettive della chiave di prova, lette con un handle di sola
    /// lettura: il percorso descrittivo è quello che il backend userebbe.
    fn security_of(isolated: &Isolated, value_path: &Path) -> platform::RegistrySecurity {
        let key = open_subkey(&isolated.key_path, KEY_READ)
            .expect("apertura di sola lettura della chiave isolata")
            .expect("la chiave isolata esiste");
        platform::registry_security(key.0, value_path).expect("protezioni leggibili")
    }

    fn real_run_value() -> Value {
        let path = display_path(KEY_PATH);
        match open_subkey(KEY_PATH, KEY_READ).expect("apertura della chiave Run reale") {
            Some(key) => read_value(&key, &path).expect("valore Run reale leggibile"),
            None => Value::Absent,
        }
    }

    fn trustee_set(security: &platform::RegistrySecurity) -> Vec<String> {
        let mut trustees: Vec<String> = security
            .entries
            .iter()
            .filter(|entry| !entry.inherit_only)
            .map(|entry| entry.trustee.clone().expect("SID convertibile"))
            .collect();
        trustees.sort();
        trustees
    }

    #[test]
    fn real_backend_creates_a_protected_key_and_is_idempotent() {
        // La preparazione lascia gli antenati e nessuna chiave della prova: è
        // il backend a doverla creare.
        let isolated = Isolated::create_key_only();
        let user = platform::current_user_sid().expect("SID dell'account corrente");
        let real_before = real_run_value();
        let location = isolated.location();
        let value_path = isolated.value_path();
        let mut backend = RunKeyBackend {
            user: user.clone(),
            location,
        };

        let snapshot = backend.snapshot().expect("snapshot iniziale");
        assert!(matches!(registry::status(&snapshot, &user), Status::Absent));

        let command = "\"C:\\Program Files\\Totum 100% $HOME\\totum-daemon.exe\"";
        let change = registry::install(&mut backend, command, &user, &value_path)
            .expect("registrazione nella chiave isolata");
        // `Created` proviene dalla disposizione di `RegCreateKeyExW`: con la
        // chiave assente nello snapshot, rigenerazione e creazione coincidono
        // solo se il registro ha restituito `REG_CREATED_NEW_KEY`.
        assert_eq!(change, InstallChange::Created);

        let snapshot = backend.snapshot().expect("snapshot dopo la creazione");
        assert!(
            snapshot
                .key
                .as_ref()
                .expect("la chiave creata è nello snapshot")
                .protected,
            "la chiave creata dal backend ha una DACL protetta"
        );
        assert_eq!(
            snapshot.value,
            Value::Present {
                kind: REG_SZ,
                text: Some(command.to_owned()),
            }
        );
        let Status::Registered { program } = registry::status(&snapshot, &user) else {
            panic!("una registrazione canonica su una catena sicura è Registered");
        };
        assert_eq!(
            program,
            PathBuf::from("C:\\Program Files\\Totum 100% $HOME\\totum-daemon.exe")
        );

        // Lettura delle protezioni effettive: la chiave creata ha una DACL
        // protetta con soli account fidati e nessuna voce ereditata.
        let security = security_of(&isolated, &value_path);
        assert_eq!(security.owner.as_deref(), Some(user.as_str()));
        assert!(security.dacl_present, "la DACL della chiave creata esiste");
        assert!(
            security.protected,
            "la DACL della chiave creata non eredita da Software\\Homun"
        );
        let mut expected = vec![
            user.clone(),
            SYSTEM_SID.to_owned(),
            ADMINISTRATORS_SID.to_owned(),
        ];
        expected.sort();
        assert_eq!(
            trustee_set(&security),
            expected,
            "la DACL protetta contiene soltanto l'utente, SYSTEM e gli amministratori"
        );
        assert!(
            security
                .entries
                .iter()
                .all(|entry| entry.kind == platform::RegistryAceKind::Allowed),
            "nessuna voce di tipo diverso è presente dopo la creazione"
        );
        assert!(
            security.entries.iter().all(|entry| !entry.inherit_only),
            "la chiave creata porta soltanto voci proprie, non ereditate"
        );

        let again = registry::install(&mut backend, command, &user, &value_path)
            .expect("seconda registrazione");
        assert_eq!(again, InstallChange::Unchanged);
        assert_eq!(
            backend
                .snapshot()
                .expect("snapshot dopo la ripetizione")
                .value,
            Value::Present {
                kind: REG_SZ,
                text: Some(command.to_owned()),
            }
        );

        // Un valore estraneo non viene sovrascritto né rimosso.
        let writable = open_subkey(&isolated.key_path, KEY_READ | KEY_SET_VALUE)
            .expect("apertura scrivibile")
            .expect("la chiave isolata esiste");
        set_value(&writable, "C:\\Other\\other.exe --flag", &value_path)
            .expect("valore estraneo scritto");
        let error = registry::install(&mut backend, command, &user, &value_path)
            .expect_err("il valore estraneo è un rifiuto");
        assert!(
            matches!(error, AutostartError::IncompatibleObject { .. }),
            "{error:?}"
        );
        let error = registry::remove(&mut backend, &user, &value_path)
            .expect_err("la rimozione di un valore estraneo è un rifiuto");
        assert!(
            matches!(error, AutostartError::IncompatibleObject { .. }),
            "{error:?}"
        );
        assert_eq!(
            read_value(&writable, &value_path).expect("valore estraneo leggibile"),
            Value::Present {
                kind: REG_SZ,
                text: Some("C:\\Other\\other.exe --flag".to_owned()),
            }
        );

        set_value(&writable, command, &value_path).expect("ripristino del valore proprio");
        assert_eq!(
            registry::remove(&mut backend, &user, &value_path).expect("rimozione"),
            RemoveChange::Removed
        );
        let snapshot = backend.snapshot().expect("snapshot finale");
        assert_eq!(snapshot.value, Value::Absent);
        assert!(matches!(registry::status(&snapshot, &user), Status::Absent));
        assert_eq!(
            registry::remove(&mut backend, &user, &value_path).expect("seconda rimozione"),
            RemoveChange::AlreadyAbsent
        );

        assert_eq!(
            real_run_value(),
            real_before,
            "la chiave Run reale e il suo valore non vengono modificati"
        );
    }

    /// Caso separato: la chiave esiste già e le protezioni vengono alterate,
    /// così il backend la incontra preesistente e deve rifiutarla.
    #[test]
    fn real_backend_refuses_a_key_another_account_can_modify() {
        let isolated = Isolated::create_key_and_value();
        let user = platform::current_user_sid().expect("SID dell'account corrente");
        let location = isolated.location();
        let value_path = isolated.value_path();

        let before = security_of(&isolated, &value_path);
        let granted = grant_write_to_builtin_users(&isolated.key_path, &user);
        let after = security_of(&isolated, &value_path);
        let altered = trustee_set(&before) != trustee_set(&after);
        if let Err(reason) = &granted {
            eprintln!("alterazione della DACL di prova non applicata: {reason}");
        }
        if !altered {
            let reason = granted
                .as_ref()
                .err()
                .map(String::as_str)
                .unwrap_or("nessun errore, ma la DACL non è cambiata");
            eprintln!(
                "prova TB-08-Windows: l'ambiente non espone la chiave isolata a `BUILTIN\\Users` \
                 ({reason}); il rifiuto basato sulla DACL non è osservabile qui e la prova \
                 dichiara l'ambiente invece di adattarsi"
            );
            return;
        }
        assert!(
            trustee_set(&after)
                .iter()
                .any(|trustee| trustee == USERS_SID),
            "la DACL alterata elenca BUILTIN\\Users fra i titolari effettivi: {:?}",
            trustee_set(&after)
        );

        let mut backend = RunKeyBackend {
            user: user.clone(),
            location,
        };
        let snapshot = backend.snapshot().expect("snapshot");
        let Status::Incompatible { detail } = registry::status(&snapshot, &user) else {
            panic!("una chiave modificabile da BUILTIN\\Users non è Registered");
        };
        assert!(detail.contains(USERS_SID), "{detail}");
        assert!(detail.contains("modificare"), "{detail}");

        let error = registry::install(
            &mut backend,
            "C:\\Totum\\totum-daemon.exe",
            &user,
            &value_path,
        )
        .expect_err("la registrazione sotto protezioni non sicure è un rifiuto");
        assert!(
            matches!(error, AutostartError::UnsafeObject { .. }),
            "{error:?}"
        );
        assert_eq!(
            backend.snapshot().expect("snapshot dopo il rifiuto").value,
            Value::Absent,
            "nessun valore viene scritto dopo il rifiuto"
        );
        assert!(
            registry::remove(&mut backend, &user, &value_path).is_err(),
            "la rimozione sotto protezioni non sicure è un rifiuto"
        );

        // Ripristo soltanto ciò che la prova ha realmente cambiato. Se il
        // ripristino fallisce, la sottochiave con l'ACE estraneo viene rimossa
        // dall'impl Drop di Isolated durante l'unwind del panic.
        restore_policy_dacl(&isolated.key_path, &user)
            .expect("ripristino della DACL di policy sulla chiave di prova");
        let restored = security_of(&isolated, &value_path);
        assert_eq!(
            trustee_set(&restored),
            trustee_set(&before),
            "la chiave di prova torna ai soli titolari di policy"
        );
        assert!(
            restored.protected,
            "la DACL ripristinata è protetta dall'ereditarietà"
        );
        assert_eq!(
            backend
                .snapshot()
                .expect("snapshot dopo il ripristino")
                .value,
            Value::Absent,
            "il ripristino delle protezioni non scrive alcun valore"
        );
    }

    /// Applica una DACL esplicita alla chiave di prova, proteggendola o
    /// lasciando l'ereditarietà. Un ambiente che non concede l'alterazione
    /// produce un errore, non un panic: la prova deve poter distinguere il
    /// proprio ambiente dal comportamento misurato.
    fn write_dacl(key_path: &str, entries: &[(&str, u32)], protect: bool) -> Result<(), String> {
        const UNPROTECTED: u32 = 0;
        const PROTECTED: u32 = PROTECTED_DACL_SECURITY_INFORMATION;
        let key = open_subkey(key_path, KEY_READ | KEY_SET_VALUE | WRITE_DAC)
            .map_err(|error| format!("apertura con WRITE_DAC: {error:?}"))?
            .ok_or_else(|| "la chiave isolata non esiste".to_owned())?;

        let mut buffer = vec![0u32; 128];
        let acl = buffer.as_mut_ptr().cast::<ACL>();
        let size = u32::try_from(buffer.len() * std::mem::size_of::<u32>())
            .expect("dimensione ACL rappresentabile");
        // SAFETY: il buffer è allineato a 4 byte e abbastanza grande per le
        // voci richieste; i SID convertiti sono liberati con `LocalFree` anche
        // sui percorsi d'errore.
        unsafe {
            if InitializeAcl(acl, size, ACL_REVISION) == 0 {
                return Err("InitializeAcl ha rifiutato l'ACL di prova".to_owned());
            }
            for (sid_text, mask) in entries {
                let text = wide(sid_text);
                let mut sid: PSID = ptr::null_mut();
                if ConvertStringSidToSidW(text.as_ptr(), &mut sid) == 0 {
                    return Err(format!("SID {sid_text} non convertibile"));
                }
                let added = AddAccessAllowedAce(acl, ACL_REVISION, *mask, sid);
                LocalFree(sid.cast());
                if added == 0 {
                    return Err(format!("voce per {sid_text} non aggiunta"));
                }
            }
            let status = SetSecurityInfo(
                key.0,
                SE_REGISTRY_KEY,
                DACL_SECURITY_INFORMATION | if protect { PROTECTED } else { UNPROTECTED },
                ptr::null_mut(),
                ptr::null_mut(),
                acl,
                ptr::null(),
            );
            if status != ERROR_SUCCESS {
                return Err(format!("SetSecurityInfo: {status}"));
            }
        }
        Ok(())
    }

    /// Concede `KEY_SET_VALUE` a `BUILTIN\Users` lasciando il controllo
    /// completo all'utente, a SYSTEM e agli amministratori per la pulizia.
    fn grant_write_to_builtin_users(key_path: &str, user: &str) -> Result<(), String> {
        write_dacl(
            key_path,
            &[
                (user, KEY_ALL_ACCESS),
                (SYSTEM_SID, KEY_ALL_ACCESS),
                (ADMINISTRATORS_SID, KEY_ALL_ACCESS),
                (USERS_SID, KEY_SET_VALUE),
            ],
            false,
        )
    }

    /// Ricostruisce la DACL di policy della chiave di prova.
    fn restore_policy_dacl(key_path: &str, user: &str) -> Result<(), String> {
        write_dacl(
            key_path,
            &[
                (user, KEY_ALL_ACCESS),
                (SYSTEM_SID, KEY_ALL_ACCESS),
                (ADMINISTRATORS_SID, KEY_ALL_ACCESS),
            ],
            true,
        )
    }
}
