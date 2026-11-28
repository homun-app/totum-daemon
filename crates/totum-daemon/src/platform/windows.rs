//! Implementazione Windows del modulo `platform`.
//!
//! La radice nasce con una DACL protetta e un solo ACE di controllo completo per
//! il SID dell'utente, passata a `CreateDirectoryW`: non esiste una fase con
//! l'eredità del profilo. La radice viene poi aperta come handle di directory e
//! ogni operazione successiva ricontrolla che il percorso coincida ancora con
//! l'handle. Le API Win32 non offrono un equivalente generale di `openat`, quindi
//! restano finestre residue fra il ricontrollo e l'apertura: la limitazione è
//! dichiarata nella documentazione.
//!
//! Nessuna funzione esegue processi esterni o modifica ACL esistenti: una DACL
//! che non corrisponde alle attese è un errore, non qualcosa da correggere.

use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr;

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES,
    ERROR_PATH_NOT_FOUND, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    LocalFree, WIN32_ERROR,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT, SE_OBJECT_TYPE, SE_REGISTRY_KEY,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSecurityDescriptorControl,
    GetTokenInformation, INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER,
    TokenElevation, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem as fs;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{
    PlatformError, RegistryAce, RegistryAceKind, RegistrySecurity, validate_nominal,
    validate_single_component,
};

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const SYSTEM_AUDIT_ACE_TYPE: u8 = 2;
const SYSTEM_ALARM_ACE_TYPE: u8 = 3;

/// Diritti che permettono di cancellare o sostituire la voce successiva, oppure
/// di modificare le protezioni. La creazione di nuove voci (`FILE_ADD_FILE`,
/// `FILE_ADD_SUBDIRECTORY`) è normale sugli antenati di sistema e non basta a
/// sostituire un oggetto esistente.
const ANCESTOR_REPLACEMENT: u32 =
    fs::DELETE | fs::FILE_DELETE_CHILD | fs::WRITE_DAC | fs::WRITE_OWNER;

const FILE_FLAGS: u32 = fs::FILE_FLAG_BACKUP_SEMANTICS | fs::FILE_FLAG_OPEN_REPARSE_POINT;

/// Radice di stato nominale dell'account.
pub(super) fn state_root() -> Result<PathBuf, PlatformError> {
    let value = std::env::var_os("LOCALAPPDATA").ok_or(PlatformError::MissingEnvironment {
        variable: "LOCALAPPDATA",
    })?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(PlatformError::InvalidEnvironment {
            variable: "LOCALAPPDATA",
            reason: "il percorso non è assoluto",
        });
    }
    Ok(path.join("Totum"))
}

/// Rilevamento dell'esecuzione elevata tramite il token del processo.
pub(super) fn is_elevated() -> Result<bool, PlatformError> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: il processo corrente è un handle valido; il token va chiuso.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(identity_error(format!(
            "apertura del token del processo fallita: {}",
            last_error()
        )));
    }
    // SAFETY: il token è stato aperto sopra e non è più usato dopo il guard.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };

    let mut elevation = TOKEN_ELEVATION::default();
    let mut size = 0u32;
    // SAFETY: il buffer ha la dimensione dichiarata e il tipo richiesto.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
    };
    if ok == 0 {
        return Err(identity_error(format!(
            "lettura del token elevato fallita: {}",
            last_error()
        )));
    }
    Ok(elevation.TokenIsElevated != 0)
}

/// SID dell'account corrente in forma di stringa.
pub(crate) fn current_user_sid() -> Result<String, PlatformError> {
    Ok(UserSid::current()?.string)
}

/// Descrittore di sicurezza applicato alla creazione di una chiave di registro.
///
/// La DACL è protetta e contiene soltanto il controllo completo dell'utente,
/// di `SYSTEM` e degli amministratori: la chiave non passa mai da uno stato in
/// cui l'eredità del genitore conceda accesso ad altri account.
pub(crate) struct RegistryCreationSecurity(SecurityDescriptor);

impl RegistryCreationSecurity {
    pub(crate) fn new() -> Result<Self, PlatformError> {
        let user = UserSid::current()?;
        let sddl = format!("O:{0}D:P(A;;KA;;;{0})(A;;KA;;;SY)(A;;KA;;;BA)", user.string);
        Ok(Self(SecurityDescriptor::from_sddl(&sddl)?))
    }

    pub(crate) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        self.0.attributes()
    }
}

/// Legge proprietario e DACL effettivi di una chiave di registro aperta.
///
/// La lettura usa l'handle realmente impiegato per le modifiche: un SID o una
/// voce non convertibili restano valori assenti e vengono rifiutati dalla
/// politica del chiamante, mai interpretati come permessi.
pub(crate) fn registry_security(
    handle: HANDLE,
    path: &Path,
) -> Result<RegistrySecurity, PlatformError> {
    let info = SecurityInfo::read_object(handle, path, SE_REGISTRY_KEY)?;
    let owner = sid_to_string(info.owner);
    if info.dacl.is_null() {
        return Ok(RegistrySecurity {
            owner,
            dacl_present: false,
            protected: info.protected,
            entries: Vec::new(),
        });
    }
    let mut entries = Vec::new();
    for index in 0..acl_size(info.dacl)? {
        let entry = ace(info.dacl, index)?;
        // SAFETY: l'indice è compreso nel numero di voci e la testa è comune.
        let header = unsafe { &*(entry.cast::<ACE_HEADER>()) };
        let kind = match header.AceType {
            ACCESS_ALLOWED_ACE_TYPE => RegistryAceKind::Allowed,
            ACCESS_DENIED_ACE_TYPE => RegistryAceKind::Denied,
            SYSTEM_AUDIT_ACE_TYPE => RegistryAceKind::Audit,
            SYSTEM_ALARM_ACE_TYPE => RegistryAceKind::Alarm,
            _ => RegistryAceKind::Other,
        };
        let (mask, trustee) = match kind {
            RegistryAceKind::Allowed
            | RegistryAceKind::Denied
            | RegistryAceKind::Audit
            | RegistryAceKind::Alarm => {
                // SAFETY: le quattro strutture condividono maschera e SID.
                let allowed = unsafe { &*(entry.cast::<ACCESS_ALLOWED_ACE>()) };
                (
                    allowed.Mask,
                    sid_to_string(
                        std::ptr::addr_of!(allowed.SidStart)
                            .cast::<c_void>()
                            .cast_mut(),
                    ),
                )
            }
            RegistryAceKind::Other => (0, None),
        };
        entries.push(RegistryAce {
            kind,
            inherit_only: u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0,
            mask,
            trustee,
        });
    }
    Ok(RegistrySecurity {
        owner,
        dacl_present: true,
        protected: info.protected,
        entries,
    })
}

/// Verifica o crea la radice e la ancora a un handle di directory.
pub(super) fn prepare_root(nominal: &Path) -> Result<VerifiedRoot, PlatformError> {
    prepare_root_with(nominal, is_elevated)
}

/// Come [`prepare_root`], con rilevamento dell'elevazione iniettabile nei test.
pub(super) fn prepare_root_with(
    nominal: &Path,
    probe: impl FnOnce() -> Result<bool, PlatformError>,
) -> Result<VerifiedRoot, PlatformError> {
    validate_nominal(nominal)?;
    let user = UserSid::current()?;
    let (base, missing) = walk_existing(nominal, &user)?;

    if probe()? {
        return Err(PlatformError::ElevatedExecution);
    }

    let sddl = format!("D:P(A;;FA;;;{})", user.string);
    let mut handle = open_directory(&base)?;
    let mut path = base;
    for name in &missing {
        let (child, child_path) = create_child_directory(&path, name, &user, &sddl)?;
        handle = child;
        path = child_path;
    }

    check_object(&handle, &path, &user, ObjectKind::Directory)?;

    Ok(VerifiedRoot {
        handle,
        path,
        user,
        sddl,
    })
}

/// Radice di stato verificata e ancorata a un handle di directory.
#[derive(Debug)]
pub struct VerifiedRoot {
    handle: OwnedHandle,
    path: PathBuf,
    user: UserSid,
    sddl: String,
}

impl VerifiedRoot {
    /// Percorso usato per la diagnostica e per i ricontrolli.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Ricontrolla che il percorso indichi ancora la directory ancorata.
    ///
    /// Va chiamato prima di ogni operazione basata su percorso: le API Win32 non
    /// offrono `openat`, quindi la finestra residua è dichiarata e non colmata.
    pub fn verify_path(&self) -> Result<(), PlatformError> {
        if attributes(&self.path)?.is_none() {
            return Err(PlatformError::PathChanged {
                path: self.path.clone(),
            });
        }
        let fresh = open_directory(&self.path)?;
        if identity(fresh.as_raw_handle(), &self.path)?
            != identity(self.handle.as_raw_handle(), &self.path)?
        {
            return Err(PlatformError::PathChanged {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    /// Crea un file nuovo con DACL protetta imposta e verificata.
    ///
    /// Un oggetto preesistente produce [`PlatformError::AlreadyExists`] e non
    /// viene seguito né modificato.
    pub fn create_file(&self, name: &OsStr) -> Result<File, PlatformError> {
        self.create_named_file(name, GENERIC_WRITE)
    }

    /// Crea un file nuovo in lettura e scrittura, con la DACL protetta.
    ///
    /// È la primitiva richiesta dal journal: il backend riceve un handle già
    /// verificato e non riapre mai il file per percorso. Un oggetto
    /// preesistente produce [`PlatformError::AlreadyExists`].
    pub(crate) fn create_read_write_file(&self, name: &OsStr) -> Result<File, PlatformError> {
        self.create_named_file(name, GENERIC_READ | GENERIC_WRITE)
    }

    fn create_named_file(&self, name: &OsStr, access: u32) -> Result<File, PlatformError> {
        let name = validate_single_component(name)?;
        self.verify_path()?;
        let path = self.path.join(name);
        let descriptor = SecurityDescriptor::from_sddl(&self.sddl)?;
        let security = descriptor.attributes();

        // SAFETY: i percorsi e il descrittore sono validi per la chiamata.
        let raw = unsafe {
            fs::CreateFileW(
                wide(&path).as_ptr(),
                access,
                0,
                &security,
                fs::CREATE_NEW,
                fs::FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            let error = last_error();
            return Err(
                if error.raw_os_error() == Some(ERROR_FILE_EXISTS as i32)
                    || error.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32)
                {
                    PlatformError::AlreadyExists { path }
                } else {
                    PlatformError::io(&path, error)
                },
            );
        }
        // SAFETY: l'handle è appena stato creato e non è usato altrove.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };

        check_object(&handle, &path, &self.user, ObjectKind::File)?;
        Ok(File::from(handle))
    }

    /// Apre un file di stato esistente in lettura e scrittura, senza crearlo.
    ///
    /// Un file assente produce [`PlatformError::FileAbsent`]; non esiste un
    /// percorso di creazione implicita. Tipo, proprietario, DACL, reparse point
    /// e assenza di collegamenti multipli sono verificati come per
    /// [`Self::open_lock_file`].
    pub(crate) fn open_existing_state_file(
        &self,
        name: &OsStr,
        require_single_link: bool,
    ) -> Result<File, PlatformError> {
        let name = validate_single_component(name)?;
        self.verify_path()?;
        let path = self.path.join(name);
        let share = fs::FILE_SHARE_READ | fs::FILE_SHARE_WRITE | fs::FILE_SHARE_DELETE;

        // SAFETY: il percorso è valido e l'handle viene chiuso dal guard.
        let raw = unsafe {
            fs::CreateFileW(
                wide(&path).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                share,
                ptr::null(),
                fs::OPEN_EXISTING,
                fs::FILE_ATTRIBUTE_NORMAL | fs::FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(file_open_error(&path, last_error()));
        }
        // SAFETY: l'handle è valido e non è usato altrove.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };

        check_object(&handle, &path, &self.user, ObjectKind::File)?;
        if require_single_link {
            self.verify_path()?;
            if file_information(handle.as_raw_handle(), &path)?.nNumberOfLinks != 1 {
                return Err(PlatformError::AclViolation {
                    path,
                    detail: "file con più collegamenti".into(),
                });
            }
        }
        Ok(File::from(handle))
    }

    /// Apre un file esistente senza seguire reparse point e ne verifica la DACL.
    ///
    /// Un file assente nella radice appena ricontrollata produce
    /// [`PlatformError::FileAbsent`], non un cambio di percorso; un componente
    /// del percorso sparito fra il ricontrollo e l'apertura resta
    /// [`PlatformError::PathChanged`]; accesso negato, reparse point, tipo,
    /// proprietario o DACL restano rifiuti specifici.
    pub fn open_file(&self, name: &OsStr) -> Result<File, PlatformError> {
        let name = validate_single_component(name)?;
        self.verify_path()?;
        let path = self.path.join(name);

        // SAFETY: il percorso è valido per la chiamata; l'handle viene chiuso dal guard.
        let raw = unsafe {
            fs::CreateFileW(
                wide(&path).as_ptr(),
                GENERIC_READ,
                fs::FILE_SHARE_READ | fs::FILE_SHARE_WRITE | fs::FILE_SHARE_DELETE,
                ptr::null(),
                fs::OPEN_EXISTING,
                fs::FILE_ATTRIBUTE_NORMAL | fs::FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(file_open_error(&path, last_error()));
        }
        // SAFETY: l'handle è valido e non è usato altrove.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };

        check_object(&handle, &path, &self.user, ObjectKind::File)?;
        Ok(File::from(handle))
    }

    /// Crea o apre il file di lock con la DACL protetta e senza seguire
    /// reparse point.
    ///
    /// Il file viene aperto in lettura e scrittura, la modalità documentata per
    /// acquisire il lock, con condivisione di lettura, scrittura ed eliminazione
    /// così che una seconda istanza possa tentare il lock e ricevere il rifiuto
    /// invece di un errore di condivisione. Se manca viene creato con la DACL
    /// protetta del proprietario; se esiste, proprietà, DACL, tipo e assenza di
    /// reparse point vengono verificati e mai riparati. Un oggetto non sicuro è
    /// un rifiuto.
    pub fn open_lock_file(&self, name: &OsStr) -> Result<File, PlatformError> {
        let name = validate_single_component(name)?;
        self.verify_path()?;
        let path = self.path.join(name);
        let share = fs::FILE_SHARE_READ | fs::FILE_SHARE_WRITE | fs::FILE_SHARE_DELETE;
        let access = GENERIC_READ | GENERIC_WRITE;

        let descriptor = SecurityDescriptor::from_sddl(&self.sddl)?;
        let security = descriptor.attributes();
        // SAFETY: il percorso e il descrittore sono validi per la chiamata.
        let mut raw = unsafe {
            fs::CreateFileW(
                wide(&path).as_ptr(),
                access,
                share,
                &security,
                fs::CREATE_NEW,
                fs::FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            let error = last_error();
            if error.raw_os_error() != Some(ERROR_FILE_EXISTS as i32)
                && error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32)
            {
                return Err(PlatformError::io(&path, error));
            }
            // SAFETY: il percorso è valido; l'handle viene chiuso dal guard.
            raw = unsafe {
                fs::CreateFileW(
                    wide(&path).as_ptr(),
                    access,
                    share,
                    ptr::null(),
                    fs::OPEN_EXISTING,
                    fs::FILE_ATTRIBUTE_NORMAL | fs::FILE_FLAG_OPEN_REPARSE_POINT,
                    ptr::null_mut(),
                )
            };
            if raw == INVALID_HANDLE_VALUE {
                return Err(file_open_error(&path, last_error()));
            }
        }
        // SAFETY: l'handle è valido e non è usato altrove.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };

        check_object(&handle, &path, &self.user, ObjectKind::File)?;
        Ok(File::from(handle))
    }

    /// Pubblica `temp` come `target` soltanto se `target` non esiste.
    ///
    /// `MoveFileExW` senza `MOVEFILE_REPLACE_EXISTING` fallisce quando la
    /// destinazione esiste: la comparsa di `target` fra la classificazione e la
    /// pubblicazione produce [`PlatformError::AlreadyExists`] e nessuna
    /// sovrascrittura.
    pub fn publish_file(&self, temp: &OsStr, target: &OsStr) -> Result<(), PlatformError> {
        let temp = validate_single_component(temp)?;
        let target = validate_single_component(target)?;
        self.verify_path()?;
        let temp_path = self.path.join(temp);
        let target_path = self.path.join(target);

        // SAFETY: i due percorsi sono stringhe UTF-16 terminate da NUL.
        let moved =
            unsafe { fs::MoveFileExW(wide(&temp_path).as_ptr(), wide(&target_path).as_ptr(), 0) };
        if moved == 0 {
            return Err(move_error(&temp_path, &target_path, last_error()));
        }
        Ok(())
    }

    /// Sostituisce atomicamente `target` esistente con `temp`.
    ///
    /// La libreria standard della toolchain fissata usa anche FileRenameInfoEx
    /// con semantica POSIX se MoveFileExW rifiuta una destinazione aperta.
    /// Un lettore vede la generazione precedente o quella nuova. Fra
    /// il ricontrollo del percorso e la chiamata resta la finestra residua
    /// dichiarata delle API basate su percorso.
    pub fn replace_file(&self, temp: &OsStr, target: &OsStr) -> Result<(), PlatformError> {
        let temp = validate_single_component(temp)?;
        let target = validate_single_component(target)?;
        self.verify_path()?;
        let temp_path = self.path.join(temp);
        let target_path = self.path.join(target);

        std::fs::rename(&temp_path, &target_path)
            .map_err(|error| move_error(&temp_path, &target_path, error))
    }

    /// Rimuove un file nella radice, senza seguirne eventuali reparse point.
    ///
    /// `DeleteFileW` elimina la voce di directory, non l'oggetto puntato da un
    /// reparse point. Un nome assente produce [`PlatformError::FileAbsent`].
    pub fn remove_file(&self, name: &OsStr) -> Result<(), PlatformError> {
        let name = validate_single_component(name)?;
        self.verify_path()?;
        let path = self.path.join(name);

        // SAFETY: il percorso è una stringa UTF-16 terminata da NUL.
        let removed = unsafe { fs::DeleteFileW(wide(&path).as_ptr()) };
        if removed == 0 {
            let error = last_error();
            return Err(match error.raw_os_error() {
                Some(code) if code == ERROR_FILE_NOT_FOUND as i32 => {
                    PlatformError::FileAbsent { path }
                }
                Some(code) if code == ERROR_PATH_NOT_FOUND as i32 => {
                    PlatformError::PathChanged { path }
                }
                _ => PlatformError::io(&path, error),
            });
        }
        Ok(())
    }

    /// Elenca i nomi delle voci della radice, senza seguirle.
    pub fn list_file_names(&self) -> Result<Vec<OsString>, PlatformError> {
        self.verify_path()?;
        let pattern = self.path.join("*");

        let mut data = fs::WIN32_FIND_DATAW::default();
        // SAFETY: il pattern è una stringa UTF-16 terminata da NUL e il buffer
        // ha il tipo richiesto.
        let first = unsafe { fs::FindFirstFileW(wide(&pattern).as_ptr(), &mut data) };
        if first == INVALID_HANDLE_VALUE {
            let error = last_error();
            return match error.raw_os_error() {
                // Una radice vuota non è un errore.
                Some(code) if code == ERROR_FILE_NOT_FOUND as i32 => Ok(Vec::new()),
                Some(code) if code == ERROR_PATH_NOT_FOUND as i32 => {
                    Err(PlatformError::PathChanged {
                        path: self.path.clone(),
                    })
                }
                _ => Err(PlatformError::io(&self.path, error)),
            };
        }
        // SAFETY: l'handle è stato aperto sopra e viene chiuso dal guard.
        let find = FindGuard(first);

        let mut names = Vec::new();
        loop {
            let name = utf16_name(&data.cFileName);
            if name != "." && name != ".." {
                names.push(OsString::from(name));
            }

            data = fs::WIN32_FIND_DATAW::default();
            // SAFETY: l'handle è valido e il buffer ha il tipo richiesto.
            if unsafe { fs::FindNextFileW(find.0, &mut data) } == 0 {
                let error = last_error();
                return match error.raw_os_error() {
                    Some(code) if code == ERROR_NO_MORE_FILES as i32 => Ok(names),
                    _ => Err(PlatformError::io(&self.path, error)),
                };
            }
        }
    }

    /// La libreria standard e `windows-sys` non offrono la sincronizzazione
    /// della voce di directory: l'esito è dichiarato non disponibile.
    pub fn sync_directory(&self) -> Result<super::DirectorySync, PlatformError> {
        Ok(super::DirectorySync::Unsupported)
    }
}

/// Sincronizza un file già scritto con `FlushFileBuffers`.
pub(super) fn sync_file(file: &File, path: &Path) -> Result<(), PlatformError> {
    file.sync_all()
        .map_err(|error| PlatformError::io(path, error))
}

/// Handle di ricerca aperto, chiuso dal `Drop`.
struct FindGuard(HANDLE);

impl Drop for FindGuard {
    fn drop(&mut self) {
        // SAFETY: l'handle proviene da `FindFirstFileW` e non è più usato.
        unsafe {
            fs::FindClose(self.0);
        }
    }
}

fn utf16_name(raw: &[u16; 260]) -> String {
    let length = raw.iter().position(|unit| *unit == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..length])
}

/// Errore di spostamento di un temporaneo su una voce di directory.
fn move_error(temp: &Path, target: &Path, error: io::Error) -> PlatformError {
    match error.raw_os_error() {
        Some(code) if code == ERROR_FILE_NOT_FOUND as i32 => PlatformError::FileAbsent {
            path: temp.to_path_buf(),
        },
        Some(code) if code == ERROR_PATH_NOT_FOUND as i32 => PlatformError::PathChanged {
            path: target.to_path_buf(),
        },
        Some(code) if code == ERROR_ALREADY_EXISTS as i32 || code == ERROR_FILE_EXISTS as i32 => {
            PlatformError::AlreadyExists {
                path: target.to_path_buf(),
            }
        }
        _ => PlatformError::io(target, error),
    }
}

/// Identificativo dell'utente corrente in forma di stringa SID.
#[derive(Clone, Debug)]
struct UserSid {
    string: String,
}

impl UserSid {
    fn current() -> Result<Self, PlatformError> {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: handle di processo valido; il token va chiuso.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(identity_error(format!(
                "apertura del token del processo fallita: {}",
                last_error()
            )));
        }
        // SAFETY: il token è stato aperto sopra e viene chiuso dal guard.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };

        let mut size = 0u32;
        // SAFETY: la prima chiamata dimensiona soltanto il buffer.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut size,
            );
        }
        if size == 0 {
            return Err(identity_error(format!(
                "dimensione del SID utente non disponibile: {}",
                last_error()
            )));
        }
        let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        // SAFETY: il buffer allineato ha almeno `size` byte e il tipo dichiarato.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast::<c_void>(),
                size,
                &mut size,
            )
        } == 0
        {
            return Err(identity_error(format!(
                "lettura del SID utente fallita: {}",
                last_error()
            )));
        }
        // SAFETY: il buffer contiene una struttura TOKEN_USER valida.
        let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let sid = token_user.User.Sid;
        if sid.is_null() {
            return Err(identity_error("SID utente assente".to_owned()));
        }
        let string = sid_to_string(sid)
            .ok_or_else(|| identity_error("conversione del SID utente fallita".to_owned()))?;
        Ok(UserSid { string })
    }
}

fn identity_error(detail: String) -> PlatformError {
    PlatformError::UnverifiableProtection {
        path: PathBuf::from("<identità dell'account>"),
        detail,
    }
}

/// Ultimo prefisso esistente e verificato, con i componenti mancanti da creare.
///
/// Il prefisso restituito contiene solo componenti già verificati: il primo
/// componente assente non viene aggiunto e compare una sola volta nel suffisso.
fn walk_existing(
    nominal: &Path,
    user: &UserSid,
) -> Result<(PathBuf, Vec<OsString>), PlatformError> {
    let components: Vec<Component<'_>> = nominal.components().collect();
    let mut existing = PathBuf::new();

    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Prefix(prefix) => {
                if !matches!(prefix.kind(), Prefix::Disk(_)) {
                    return Err(PlatformError::invalid(
                        nominal,
                        "sono ammessi soltanto percorsi su un volume locale",
                    ));
                }
                existing.push(prefix.as_os_str());
            }
            Component::RootDir => existing.push(component.as_os_str()),
            Component::Normal(name) => {
                let candidate = existing.join(name);
                let Some(attributes) = attributes(&candidate)? else {
                    let missing = components[index..]
                        .iter()
                        .filter_map(|component| match component {
                            Component::Normal(value) => Some(value.to_os_string()),
                            _ => None,
                        })
                        .collect();
                    return Ok((existing, missing));
                };
                if attributes & fs::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return Err(PlatformError::ReparsePointRefused { path: candidate });
                }
                if attributes & fs::FILE_ATTRIBUTE_DIRECTORY == 0 {
                    return Err(PlatformError::NotADirectory { path: candidate });
                }
                check_ancestor(&candidate, user)?;
                existing = candidate;
            }
            Component::CurDir | Component::ParentDir => {
                return Err(PlatformError::invalid(
                    nominal,
                    "componente non ammesso nel percorso",
                ));
            }
        }
    }

    Ok((existing, Vec::new()))
}

/// Verifica che nessun ACE conceda a un altro account di sostituire la voce
/// successiva o di modificarne le protezioni.
fn check_ancestor(path: &Path, user: &UserSid) -> Result<(), PlatformError> {
    let handle = open_directory(path)?;
    let security = SecurityInfo::read(handle.as_raw_handle(), path)?;
    let owner = sid_to_string(security.owner)
        .ok_or_else(|| unverifiable(path, "proprietario non convertibile in SID"))?;
    if owner != user.string && !is_system_principal(&owner) {
        return Err(PlatformError::WrongOwner {
            path: path.to_path_buf(),
            expected: format!("{} o un account di sistema", user.string),
            actual: owner,
        });
    }

    if security.dacl.is_null() {
        return Err(PlatformError::AclViolation {
            path: path.to_path_buf(),
            detail: "assenza di DACL: accesso completo a tutti".to_owned(),
        });
    }
    let dacl: *const ACL = security.dacl;
    for index in 0..acl_size(dacl)? {
        // SAFETY: l'indice è compreso nel numero di ACE letto sopra.
        let entry = ace(dacl, index)?;
        let header = unsafe { &*(entry.cast::<ACE_HEADER>()) };
        match header.AceType {
            ACCESS_DENIED_ACE_TYPE | SYSTEM_AUDIT_ACE_TYPE | SYSTEM_ALARM_ACE_TYPE => continue,
            ACCESS_ALLOWED_ACE_TYPE => {}
            _ => {
                return Err(unverifiable(
                    path,
                    "tipo di ACE non interpretabile sulla catena degli antenati",
                ));
            }
        }
        let allowed = unsafe { &*(entry.cast::<ACCESS_ALLOWED_ACE>()) };
        if allowed.Mask & ANCESTOR_REPLACEMENT == 0 {
            continue;
        }
        let trustee = sid_to_string(
            std::ptr::addr_of!(allowed.SidStart)
                .cast::<c_void>()
                .cast_mut(),
        )
        .ok_or_else(|| unverifiable(path, "SID di una voce ACL non convertibile"))?;
        if trustee == user.string || is_system_principal(&trustee) {
            continue;
        }
        return Err(PlatformError::AclViolation {
            path: path.to_path_buf(),
            detail: format!(
                "l'account {trustee} può sostituire la voce successiva o modificarne le protezioni"
            ),
        });
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ObjectKind {
    Directory,
    File,
}

/// Verifica proprietà, DACL protetta e singolo ACE di controllo completo.
fn check_object(
    handle: &OwnedHandle,
    path: &Path,
    user: &UserSid,
    kind: ObjectKind,
) -> Result<(), PlatformError> {
    let info = file_information(handle.as_raw_handle(), path)?;
    if info.dwFileAttributes & fs::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PlatformError::ReparsePointRefused {
            path: path.to_path_buf(),
        });
    }
    let is_directory = info.dwFileAttributes & fs::FILE_ATTRIBUTE_DIRECTORY != 0;
    match (kind, is_directory) {
        (ObjectKind::Directory, false) => {
            return Err(PlatformError::NotADirectory {
                path: path.to_path_buf(),
            });
        }
        (ObjectKind::File, true) => {
            return Err(PlatformError::NotARegularFile {
                path: path.to_path_buf(),
            });
        }
        _ => {}
    }

    let security = SecurityInfo::read(handle.as_raw_handle(), path)?;
    let owner = sid_to_string(security.owner)
        .ok_or_else(|| unverifiable(path, "proprietario non convertibile in SID"))?;
    if owner != user.string {
        return Err(PlatformError::WrongOwner {
            path: path.to_path_buf(),
            expected: user.string.clone(),
            actual: owner,
        });
    }
    if !security.protected {
        return Err(PlatformError::AclViolation {
            path: path.to_path_buf(),
            detail: "DACL non protetta: l'eredità può modificare i permessi".to_owned(),
        });
    }
    if security.dacl.is_null() {
        return Err(PlatformError::AclViolation {
            path: path.to_path_buf(),
            detail: "assenza di DACL".to_owned(),
        });
    }
    let dacl: *const ACL = security.dacl;
    if acl_size(dacl)? != 1 {
        return Err(PlatformError::AclViolation {
            path: path.to_path_buf(),
            detail: "la DACL deve contenere un solo ACE per il SID dell'utente".to_owned(),
        });
    }
    // SAFETY: la DACL ha esattamente una voce, verificata sopra.
    let entry = ace(dacl, 0)?;
    let header = unsafe { &*(entry.cast::<ACE_HEADER>()) };
    if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
        return Err(PlatformError::AclViolation {
            path: path.to_path_buf(),
            detail: "la DACL contiene una voce che non è di accesso consentito".to_owned(),
        });
    }
    let allowed = unsafe { &*(entry.cast::<ACCESS_ALLOWED_ACE>()) };
    if allowed.Mask != fs::FILE_ALL_ACCESS {
        return Err(PlatformError::AclViolation {
            path: path.to_path_buf(),
            detail: format!(
                "l'ACE dell'utente non ha controllo completo (mask {:#x})",
                allowed.Mask
            ),
        });
    }
    let trustee = sid_to_string(
        std::ptr::addr_of!(allowed.SidStart)
            .cast::<c_void>()
            .cast_mut(),
    )
    .ok_or_else(|| unverifiable(path, "SID dell'ACE non convertibile"))?;
    if trustee != user.string {
        return Err(PlatformError::AclViolation {
            path: path.to_path_buf(),
            detail: format!("l'ACE di controllo completo appartiene a {trustee}"),
        });
    }
    Ok(())
}

/// Crea un componente mancante con DACL protetta già alla creazione, poi lo
/// verifica e lo ancora; un oggetto comparso nel frattempo è trattato come
/// preesistente e verificato senza riparazioni.
fn create_child_directory(
    parent_path: &Path,
    name: &OsStr,
    user: &UserSid,
    sddl: &str,
) -> Result<(OwnedHandle, PathBuf), PlatformError> {
    let path = parent_path.join(name);
    let descriptor = SecurityDescriptor::from_sddl(sddl)?;
    let security = descriptor.attributes();

    // `CreateDirectoryW` è l'API per la creazione senza template; sfogliare una
    // directory modello con `CreateDirectoryExW` non è necessario ed è escluso.
    // SAFETY: il percorso e il descrittore sono validi per la chiamata.
    let created = unsafe { fs::CreateDirectoryW(wide(&path).as_ptr(), &security) };
    if created == 0 {
        let error = last_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(PlatformError::io(&path, error));
        }
    }

    let handle = open_directory(&path)?;
    check_object(&handle, &path, user, ObjectKind::Directory)?;

    let fresh = open_directory(&path)?;
    if identity(handle.as_raw_handle(), &path)? != identity(fresh.as_raw_handle(), &path)? {
        return Err(PlatformError::PathChanged { path });
    }

    Ok((handle, path))
}

/// Apre un oggetto esistente con `OPEN_REPARSE_POINT` e ne verifica il tipo.
fn open_directory(path: &Path) -> Result<OwnedHandle, PlatformError> {
    // SAFETY: il percorso è valido; l'handle è chiuso dal guard.
    let raw = unsafe {
        fs::CreateFileW(
            wide(path).as_ptr(),
            fs::FILE_LIST_DIRECTORY | fs::FILE_READ_ATTRIBUTES | fs::READ_CONTROL | fs::SYNCHRONIZE,
            fs::FILE_SHARE_READ | fs::FILE_SHARE_WRITE | fs::FILE_SHARE_DELETE,
            ptr::null(),
            fs::OPEN_EXISTING,
            FILE_FLAGS,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(open_error(path, last_error()));
    }
    // SAFETY: l'handle è valido e non è usato altrove.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let info = file_information(handle.as_raw_handle(), path)?;
    if info.dwFileAttributes & fs::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PlatformError::ReparsePointRefused {
            path: path.to_path_buf(),
        });
    }
    if info.dwFileAttributes & fs::FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(PlatformError::NotADirectory {
            path: path.to_path_buf(),
        });
    }
    Ok(handle)
}

fn is_system_principal(sid: &str) -> bool {
    matches!(
        sid,
        "S-1-5-18" | "S-1-5-19" | "S-1-5-20" | "S-1-5-6" | "S-1-5-32-544" | "S-1-3-0" | "S-1-3-4"
    ) || sid.starts_with("S-1-5-80-")
        || sid.starts_with("S-1-5-83-")
}

fn attributes(path: &Path) -> Result<Option<u32>, PlatformError> {
    // SAFETY: il percorso è una stringa UTF-16 terminata da NUL.
    let value = unsafe { fs::GetFileAttributesW(wide(path).as_ptr()) };
    if value != u32::MAX {
        return Ok(Some(value));
    }
    let error = last_error();
    match error.raw_os_error() {
        Some(code)
            if code == ERROR_FILE_NOT_FOUND as i32 || code == ERROR_PATH_NOT_FOUND as i32 =>
        {
            Ok(None)
        }
        _ => Err(PlatformError::io(path, error)),
    }
}

fn file_information(
    handle: HANDLE,
    path: &Path,
) -> Result<fs::BY_HANDLE_FILE_INFORMATION, PlatformError> {
    let mut information: fs::BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    // SAFETY: l'handle è valido e la struttura ha la dimensione corretta.
    if unsafe { fs::GetFileInformationByHandle(handle, &mut information) } == 0 {
        return Err(PlatformError::io(path, last_error()));
    }
    Ok(information)
}

fn identity(handle: HANDLE, path: &Path) -> Result<(u32, u64), PlatformError> {
    let info = file_information(handle, path)?;
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok((info.dwVolumeSerialNumber, index))
}

fn acl_size(dacl: *const ACL) -> Result<u32, PlatformError> {
    let mut information: ACL_SIZE_INFORMATION = unsafe { zeroed() };
    // SAFETY: la DACL proviene da GetSecurityInfo e il buffer è della classe richiesta.
    if unsafe {
        GetAclInformation(
            dacl,
            (&raw mut information).cast::<c_void>(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return Err(PlatformError::UnverifiableProtection {
            path: PathBuf::from("<DACL>"),
            detail: format!("dimensioni della DACL illeggibili: {}", last_error()),
        });
    }
    Ok(information.AceCount)
}

fn ace(dacl: *const ACL, index: u32) -> Result<*mut c_void, PlatformError> {
    let mut entry: *mut c_void = ptr::null_mut();
    // SAFETY: la DACL è valida e `index` è compreso nel numero di voci.
    if unsafe { GetAce(dacl, index, &mut entry) } == 0 || entry.is_null() {
        return Err(PlatformError::UnverifiableProtection {
            path: PathBuf::from("<DACL>"),
            detail: format!("voce ACL illeggibile: {}", last_error()),
        });
    }
    Ok(entry)
}

/// Descrittore di sicurezza autorelativo applicato alla creazione.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> Result<Self, PlatformError> {
        let text: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: il testo è terminato da NUL; il descrittore va liberato.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if ok == 0 || descriptor.is_null() {
            return Err(PlatformError::UnverifiableProtection {
                path: PathBuf::from("<DACL>"),
                detail: format!(
                    "creazione della DACL protetta non disponibile: {}",
                    last_error()
                ),
            });
        }
        Ok(SecurityDescriptor(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: il descrittore è stato allocato dall'API e non è più usato.
        unsafe {
            LocalFree(self.0);
        }
    }
}

/// Lettura del proprietario e della DACL effettivi, con la protezione dichiarata.
struct SecurityInfo {
    owner: PSID,
    dacl: *mut ACL,
    protected: bool,
    _descriptor: PSECURITY_DESCRIPTOR,
}

impl SecurityInfo {
    fn read(handle: HANDLE, path: &Path) -> Result<Self, PlatformError> {
        Self::read_object(handle, path, SE_FILE_OBJECT)
    }

    fn read_object(
        handle: HANDLE,
        path: &Path,
        object_type: SE_OBJECT_TYPE,
    ) -> Result<Self, PlatformError> {
        let mut owner: PSID = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: l'handle è valido e il descrittore va liberato dal Drop.
        let status: WIN32_ERROR = unsafe {
            GetSecurityInfo(
                handle,
                object_type,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 || descriptor.is_null() {
            return Err(unverifiable(
                path,
                &format!(
                    "lettura delle protezioni fallita: {}",
                    io::Error::from_raw_os_error(status as i32)
                ),
            ));
        }
        let mut info = SecurityInfo {
            owner,
            dacl,
            protected: false,
            _descriptor: descriptor,
        };
        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: il descrittore è valido fino al Drop.
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
            return Err(unverifiable(
                path,
                &format!("controllo del descrittore illeggibile: {}", last_error()),
            ));
        }
        info.protected = control & SE_DACL_PROTECTED != 0;
        Ok(info)
    }
}

impl Drop for SecurityInfo {
    fn drop(&mut self) {
        // SAFETY: il descrittore è stato allocato da GetSecurityInfo.
        unsafe {
            LocalFree(self._descriptor);
        }
    }
}

fn sid_to_string(sid: PSID) -> Option<String> {
    if sid.is_null() {
        return None;
    }
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: il SID è valido; la stringa va liberata con LocalFree.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
        return None;
    }
    let mut length = 0usize;
    // SAFETY: la stringa è terminata da NUL e allocata dall'API.
    unsafe {
        while *text.add(length) != 0 {
            length += 1;
        }
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast::<c_void>());
        Some(value)
    }
}

fn unverifiable(path: &Path, detail: &str) -> PlatformError {
    PlatformError::UnverifiableProtection {
        path: path.to_path_buf(),
        detail: detail.to_owned(),
    }
}

/// Errore di apertura di una directory ancorata o del suo percorso.
///
/// Una directory assente non è mai un'assenza ammessa: l'oggetto richiesto
/// esisteva al ricontrollo, quindi `ERROR_FILE_NOT_FOUND` e
/// `ERROR_PATH_NOT_FOUND` indicano che il percorso è cambiato.
fn open_error(path: &Path, error: io::Error) -> PlatformError {
    match error.raw_os_error() {
        Some(code)
            if code == ERROR_FILE_NOT_FOUND as i32 || code == ERROR_PATH_NOT_FOUND as i32 =>
        {
            PlatformError::PathChanged {
                path: path.to_path_buf(),
            }
        }
        _ => PlatformError::io(path, error),
    }
}

/// Errore di apertura di un file esistente sotto una radice già ricontrollata.
///
/// `ERROR_FILE_NOT_FOUND` è l'assenza del file richiesto: la radice è stata
/// verificata poco prima, quindi non è un cambio di percorso.
/// `ERROR_PATH_NOT_FOUND` indica invece che un componente del percorso è
/// sparito fra il ricontrollo e l'apertura e resta un cambio di percorso.
/// Ogni altro errore, compreso l'accesso negato, è un rifiuto di sistema.
fn file_open_error(path: &Path, error: io::Error) -> PlatformError {
    match error.raw_os_error() {
        Some(code) if code == ERROR_FILE_NOT_FOUND as i32 => PlatformError::FileAbsent {
            path: path.to_path_buf(),
        },
        Some(code) if code == ERROR_PATH_NOT_FOUND as i32 => PlatformError::PathChanged {
            path: path.to_path_buf(),
        },
        _ => PlatformError::io(path, error),
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn last_error() -> io::Error {
    // SAFETY: GetLastError non ha precondizioni e non modifica lo stato.
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    fn scratch(label: &str) -> PathBuf {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("radice del workspace")
            .join("target/platform-tests");
        std::fs::create_dir_all(&base).expect("directory di prova creabile");
        let path = base.join(format!(
            "{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).expect("radice di prova creabile");
        path
    }

    /// Rilegge la DACL effettiva e la confronta con le attese.
    fn assert_protected_owner_only(path: &Path, user: &UserSid) {
        let handle = open_directory(path).expect("directory riapribile per la verifica");
        let info = SecurityInfo::read(handle.as_raw_handle(), path).expect("DACL leggibile");
        assert!(
            info.protected,
            "la DACL di {} deve essere protetta",
            path.display()
        );
        assert!(
            !info.dacl.is_null(),
            "la DACL di {} deve esistere",
            path.display()
        );
        let count = acl_size(info.dacl).expect("dimensione della DACL");
        assert_eq!(count, 1, "{} deve avere un solo ACE", path.display());
        let entry = ace(info.dacl, 0).expect("prima voce ACL");
        // SAFETY: la voce proviene da GetAce su una DACL valida.
        let header = unsafe { &*(entry.cast::<ACE_HEADER>()) };
        assert_eq!(header.AceType, ACCESS_ALLOWED_ACE_TYPE);
        // SAFETY: la voce è di accesso consentito, verificato sopra.
        let allowed = unsafe { &*(entry.cast::<ACCESS_ALLOWED_ACE>()) };
        assert_eq!(allowed.Mask, fs::FILE_ALL_ACCESS);
        let trustee = sid_to_string(
            std::ptr::addr_of!(allowed.SidStart)
                .cast::<c_void>()
                .cast_mut(),
        )
        .expect("SID della voce leggibile");
        assert_eq!(trustee, user.string);
    }

    #[test]
    fn file_open_error_distinguishes_absence_from_a_changed_path() {
        let path = Path::new("C:\\Totum\\config.json");
        assert!(matches!(
            file_open_error(
                path,
                io::Error::from_raw_os_error(ERROR_FILE_NOT_FOUND as i32)
            ),
            PlatformError::FileAbsent { .. }
        ));
        assert!(matches!(
            file_open_error(
                path,
                io::Error::from_raw_os_error(ERROR_PATH_NOT_FOUND as i32)
            ),
            PlatformError::PathChanged { .. }
        ));
        // ERROR_ACCESS_DENIED (5): un accesso negato non è mai assenza.
        match file_open_error(path, io::Error::from_raw_os_error(5)) {
            PlatformError::Io { source, .. } => assert_eq!(source.raw_os_error(), Some(5)),
            other => panic!("errore inatteso: {other:?}"),
        }
    }

    #[test]
    fn directory_open_error_treats_a_missing_component_as_a_changed_path() {
        let path = Path::new("C:\\Totum");
        for code in [ERROR_FILE_NOT_FOUND as i32, ERROR_PATH_NOT_FOUND as i32] {
            assert!(matches!(
                open_error(path, io::Error::from_raw_os_error(code)),
                PlatformError::PathChanged { .. }
            ));
        }
    }

    #[test]
    fn open_file_reports_absence_and_refuses_a_replaced_root() {
        let base = scratch("win-open");
        let nominal = base.join("Totum");
        let root = prepare_root_with(&nominal, || Ok(false)).expect("radice creabile");

        assert!(matches!(
            root.open_file(OsStr::new("config.json")),
            Err(PlatformError::FileAbsent { .. })
        ));

        let moved = base.join("moved");
        std::fs::rename(&nominal, &moved).expect("radice rinominata");
        std::fs::create_dir(&nominal).expect("radice sostituita");
        assert!(matches!(
            root.open_file(OsStr::new("config.json")),
            Err(PlatformError::PathChanged { .. })
        ));
    }

    #[test]
    fn walk_existing_returns_only_the_verified_prefix() {
        let base = scratch("win-walk");
        let user = UserSid::current().expect("SID utente leggibile");

        let nominal = base.join("Totum");
        let (existing, missing) = walk_existing(&nominal, &user).expect("prefisso camminabile");
        assert_eq!(
            existing, base,
            "il prefisso esistente non deve includere il componente mancante"
        );
        assert_eq!(missing, vec![OsString::from("Totum")]);

        let nested = base.join("a/b/Totum");
        let (existing, missing) = walk_existing(&nested, &user).expect("prefisso camminabile");
        assert_eq!(existing, base);
        assert_eq!(
            missing,
            vec![
                OsString::from("a"),
                OsString::from("b"),
                OsString::from("Totum")
            ],
            "il suffisso deve contenere ogni componente mancante una sola volta"
        );
    }

    #[test]
    fn first_creation_builds_each_component_with_a_protected_dacl() {
        let base = scratch("win-create");
        let nominal = base.join("a/b/Totum");

        let root = prepare_root_with(&nominal, || Ok(false)).expect("radice creabile");
        let user = root.user.clone();
        assert_eq!(root.path(), nominal.as_path());

        for created in [base.join("a"), base.join("a/b"), nominal.clone()] {
            assert!(created.is_dir(), "{} deve esistere", created.display());
            assert_protected_owner_only(&created, &user);
        }
    }

    #[test]
    fn reuse_keeps_the_existing_prefix_and_root() {
        let base = scratch("win-reuse");
        let nominal = base.join("Totum");

        let base_handle = open_directory(&base).expect("prefisso apribile");
        let base_identity =
            identity(base_handle.as_raw_handle(), &base).expect("identità del prefisso");
        drop(base_handle);

        let first = prepare_root_with(&nominal, || Ok(false)).expect("prima creazione");
        let first_identity =
            identity(first.handle.as_raw_handle(), &nominal).expect("identità della radice");
        drop(first);

        let second = prepare_root_with(&nominal, || Ok(false)).expect("seconda preparazione");
        let second_identity =
            identity(second.handle.as_raw_handle(), &nominal).expect("identità della radice");
        drop(second);

        let base_handle = open_directory(&base).expect("prefisso apribile");
        let base_after =
            identity(base_handle.as_raw_handle(), &base).expect("identità del prefisso");
        drop(base_handle);

        assert_eq!(first_identity, second_identity, "la radice viene riusata");
        assert_eq!(base_identity, base_after, "il prefisso non viene ricreato");
    }
}
