//! Implementazione Unix del modulo `platform` (macOS e Linux).
//!
//! Tutte le operazioni sulla radice usano il descrittore aperto una volta con
//! `O_DIRECTORY | O_NOFOLLOW` e le varianti `*at`; un percorso che non coincide
//! più con il descrittore produce un errore esplicito.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{self as rfs, AtFlags, Mode, OFlags, RenameFlags};
use rustix::io::Errno;

use super::{EntryInfo, EntryKind, PlatformError, validate_nominal, validate_single_component};

#[cfg(target_os = "linux")]
use super::linux_acl;
#[cfg(target_os = "macos")]
use super::macos_acl;

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const TYPE_MASK: u32 = 0o170_000;
const TYPE_DIRECTORY: u32 = 0o040_000;
const TYPE_REGULAR: u32 = 0o100_000;
const TYPE_SYMLINK: u32 = 0o120_000;
const STICKY: u32 = 0o1000;
const GROUP_OR_OTHER_WRITE: u32 = 0o022;

/// Radice di stato nominale dell'account.
pub(super) fn state_root() -> Result<PathBuf, PlatformError> {
    #[cfg(target_os = "macos")]
    {
        Ok(home()?.join("Library/Application Support/Totum"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let base = match std::env::var_os("XDG_STATE_HOME") {
            Some(value) => {
                let path = PathBuf::from(&value);
                if !path.is_absolute() {
                    return Err(PlatformError::InvalidEnvironment {
                        variable: "XDG_STATE_HOME",
                        reason: "il percorso non è assoluto",
                    });
                }
                path
            }
            None => home()?.join(".local/state"),
        };
        Ok(base.join("totum"))
    }
}

fn home() -> Result<PathBuf, PlatformError> {
    let value =
        std::env::var_os("HOME").ok_or(PlatformError::MissingEnvironment { variable: "HOME" })?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(PlatformError::InvalidEnvironment {
            variable: "HOME",
            reason: "il percorso non è assoluto",
        });
    }
    Ok(path)
}

/// Rilevamento dell'esecuzione elevata: identificativo effettivo uguale a 0.
pub(super) fn is_elevated() -> Result<bool, PlatformError> {
    Ok(super::euid() == 0)
}

/// Verifica o crea la radice e la ancora al descrittore aperto.
pub(super) fn prepare_root(nominal: &Path) -> Result<VerifiedRoot, PlatformError> {
    prepare_root_with(nominal, is_elevated)
}

/// Come [`prepare_root`], con rilevamento dell'elevazione iniettabile nei test.
pub(super) fn prepare_root_with(
    nominal: &Path,
    probe: impl FnOnce() -> Result<bool, PlatformError>,
) -> Result<VerifiedRoot, PlatformError> {
    validate_nominal(nominal)?;

    let (existing, missing) = walk_existing(nominal)?;
    let base_path = existing
        .canonicalize()
        .map_err(|error| PlatformError::io(&existing, error))?;
    verify_ancestors(&base_path, missing.first().map(OsString::as_os_str))?;

    if probe()? {
        return Err(PlatformError::ElevatedExecution);
    }

    let mut directory = open_directory(&base_path)?;
    let mut path = base_path;
    for component in &missing {
        let (child, child_path) = enter_child_directory(&directory, &path, component)?;
        directory = child;
        path = child_path;
    }

    let stat = fstat(&directory, &path)?;
    verify_root(&stat, &path)?;
    check_acl(directory.as_fd(), &path, super::AclPolicy::OwnerOnly)?;

    Ok(VerifiedRoot {
        directory,
        path,
        device: stat.device,
        inode: stat.inode,
    })
}

/// Verifica o crea una directory che contiene oggetti dell'utente, senza
/// imporre la modalità esatta 0700 a una directory standard preesistente.
///
/// La directory finale e gli antenati devono appartenere all'utente (o a root,
/// per gli antenati) e non essere scrivibili da gruppo o altri. I componenti
/// mancanti vengono creati con modalità 0700; una directory preesistente non
/// viene mai riparata e un collegamento simbolico sull'ultimo componente è
/// rifiutato. È la politica necessaria per `~/Library/LaunchAgents` e per
/// `~/.config/systemd/user`, che possono preesistere con modalità 0755.
pub(crate) fn prepare_containing_directory(nominal: &Path) -> Result<VerifiedRoot, PlatformError> {
    validate_nominal(nominal)?;

    let (existing, missing) = walk_existing(nominal)?;
    let base_path = existing
        .canonicalize()
        .map_err(|error| PlatformError::io(&existing, error))?;
    verify_ancestors(&base_path, missing.first().map(OsString::as_os_str))?;

    if is_elevated()? {
        return Err(PlatformError::ElevatedExecution);
    }

    let mut directory = open_directory(&base_path)?;
    let mut path = base_path;
    for component in &missing {
        let (child, child_path) = enter_child_directory(&directory, &path, component)?;
        directory = child;
        path = child_path;
    }

    let stat = fstat(&directory, &path)?;
    verify_containing(&stat, &path)?;
    check_acl(
        directory.as_fd(),
        &path,
        super::AclPolicy::DirectoryReplacement,
    )?;

    Ok(VerifiedRoot {
        directory,
        path,
        device: stat.device,
        inode: stat.inode,
    })
}

/// Apre in sola lettura una directory contenente oggetti dell'utente.
///
/// Restituisce `None` se la directory o un suo antenato manca: la
/// classificazione dello stato non crea nulla. Un oggetto non sicuro produce
/// un errore, mai una riparazione.
pub(crate) fn open_containing_directory(
    nominal: &Path,
) -> Result<Option<VerifiedRoot>, PlatformError> {
    validate_nominal(nominal)?;

    let (existing, missing) = walk_existing(nominal)?;
    let base_path = existing
        .canonicalize()
        .map_err(|error| PlatformError::io(&existing, error))?;
    verify_ancestors(&base_path, missing.first().map(OsString::as_os_str))?;
    if !missing.is_empty() {
        return Ok(None);
    }

    let directory = open_directory(&base_path)?;
    let stat = fstat(&directory, &base_path)?;
    verify_containing(&stat, &base_path)?;
    check_acl(
        directory.as_fd(),
        &base_path,
        super::AclPolicy::DirectoryReplacement,
    )?;

    Ok(Some(VerifiedRoot {
        directory,
        path: base_path,
        device: stat.device,
        inode: stat.inode,
    }))
}

/// Radice di stato verificata e ancorata a un descrittore di directory.
#[derive(Debug)]
pub struct VerifiedRoot {
    directory: OwnedFd,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl VerifiedRoot {
    /// Percorso fisico usato per la diagnostica e per i ricontrolli.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Verifica che il percorso coincida ancora con la directory ancorata.
    ///
    /// Serve dopo la preparazione e prima di operazioni basate su percorso: se
    /// un antenato è stato rinominato, il confronto dispositivo/inode fallisce.
    pub fn verify_path(&self) -> Result<(), PlatformError> {
        let stat =
            rfs::statat(rfs::CWD, &self.path, AtFlags::SYMLINK_NOFOLLOW).map_err(|errno| {
                if errno == Errno::NOENT {
                    PlatformError::PathChanged {
                        path: self.path.clone(),
                    }
                } else {
                    PlatformError::io(&self.path, errno)
                }
            })?;
        let stat = EntryStat::from(&stat);
        if stat.device != self.device || stat.inode != self.inode {
            return Err(PlatformError::PathChanged {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    /// Crea un file nuovo con modalità 0600 imposta e verificata.
    ///
    /// Il nome deve essere un singolo componente. Un oggetto preesistente, anche
    /// un collegamento simbolico, produce [`PlatformError::AlreadyExists`] e
    /// non viene seguito né modificato.
    pub fn create_file(&self, name: &OsStr) -> Result<File, PlatformError> {
        self.create_named_file(name, OFlags::WRONLY)
    }

    /// Crea un file nuovo in lettura e scrittura, con le stesse garanzie di
    /// [`Self::create_file`].
    ///
    /// È la primitiva richiesta dal journal: il backend riceve un descrittore
    /// già verificato e non riapre mai il file per percorso. Un oggetto
    /// preesistente produce [`PlatformError::AlreadyExists`].
    pub(crate) fn create_read_write_file(&self, name: &OsStr) -> Result<File, PlatformError> {
        self.create_named_file(name, OFlags::RDWR)
    }

    fn create_named_file(&self, name: &OsStr, access: OFlags) -> Result<File, PlatformError> {
        let name = validate_single_component(name)?;
        let path = self.path.join(name);
        let flags = access | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let descriptor = rfs::openat(&self.directory, name, flags, Mode::RUSR | Mode::WUSR)
            .map_err(|errno| {
                if errno == Errno::EXIST {
                    PlatformError::AlreadyExists { path: path.clone() }
                } else {
                    map_open_error(&path, errno)
                }
            })?;

        // L'umask può solo togliere bit: la modalità effettiva va imposta e
        // verificata anche quando la creazione è avvenuta più restrittiva.
        rfs::fchmod(&descriptor, Mode::RUSR | Mode::WUSR)
            .map_err(|errno| PlatformError::io(&path, errno))?;
        let stat = fstat(&descriptor, &path)?;
        verify_file(&stat, &path)?;
        check_acl(descriptor.as_fd(), &path, super::AclPolicy::OwnerOnly)?;

        Ok(File::from(descriptor))
    }

    /// Apre un file esistente senza seguire collegamenti e ne verifica le
    /// garanzie: tipo regolare, proprietario l'utente, modalità esattamente 0600.
    ///
    /// Un file assente nella radice ancorata produce
    /// [`PlatformError::FileAbsent`], non un cambio di percorso; collegamenti,
    /// tipo errato, proprietario o modalità restano rifiuti specifici.
    ///
    /// L'apertura è non bloccante: una FIFO senza scrittore non deve sospendere
    /// la chiamata. Il tipo viene verificato sul descrittore effettivamente
    /// aperto e solo dopo il descrittore torna bloccante.
    pub fn open_file(&self, name: &OsStr) -> Result<File, PlatformError> {
        let name = validate_single_component(name)?;
        let path = self.path.join(name);
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let descriptor = rfs::openat(&self.directory, name, flags, Mode::empty())
            .map_err(|errno| map_file_open_error(&path, errno))?;
        let stat = fstat(&descriptor, &path)?;
        verify_file(&stat, &path)?;
        check_acl(descriptor.as_fd(), &path, super::AclPolicy::OwnerOnly)?;

        // Una FIFO senza scrittore si apre subito con `O_NONBLOCK`; un file
        // regolare non ne è influenzato, ma lo stato viene comunque ripristinato
        // prima di restituirlo al chiamante.
        rfs::fcntl_setfl(&descriptor, OFlags::empty())
            .map_err(|errno| PlatformError::io(&path, errno))?;
        Ok(File::from(descriptor))
    }

    /// Crea o apre il file di lock con le stesse garanzie dei file di stato.
    ///
    /// Il file viene aperto in lettura e scrittura, la modalità documentata per
    /// acquisire il lock. Se manca viene creato in modo esclusivo con modalità
    /// 0600 imposta; se esiste viene aperto senza seguire collegamenti e il tipo
    /// regolare, il proprietario, la modalità esattamente 0600 e l'ACL vengono
    /// verificati e mai riparati. Un oggetto di tipo diverso o un collegamento
    /// produce un rifiuto. L'apertura è non bloccante e il descrittore torna
    /// bloccante prima di essere restituito.
    pub fn open_lock_file(&self, name: &OsStr) -> Result<File, PlatformError> {
        let name = validate_single_component(name)?;
        let path = self.path.join(name);
        let create =
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let descriptor = match rfs::openat(&self.directory, name, create, Mode::RUSR | Mode::WUSR) {
            Ok(descriptor) => {
                // L'umask può solo togliere bit: la modalità viene imposta.
                rfs::fchmod(&descriptor, Mode::RUSR | Mode::WUSR)
                    .map_err(|errno| PlatformError::io(&path, errno))?;
                descriptor
            }
            Err(Errno::EXIST) => {
                let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
                rfs::openat(&self.directory, name, flags, Mode::empty())
                    .map_err(|errno| lock_open_error(&path, errno))?
            }
            Err(errno) => return Err(map_open_error(&path, errno)),
        };

        self.finish_state_file(descriptor, path, false)
    }

    /// Apre un file di stato esistente in lettura e scrittura, senza crearlo.
    ///
    /// Un file assente produce [`PlatformError::FileAbsent`]; non esiste un
    /// percorso di creazione implicita. Tipo, proprietario, modalità, ACL e
    /// assenza di collegamenti multipli sono verificati come per
    /// [`Self::open_lock_file`]. È la primitiva con cui il journal apre un
    /// file preesistente che non deve essere inizializzato per omissione.
    pub(crate) fn open_existing_state_file(
        &self,
        name: &OsStr,
        require_single_link: bool,
    ) -> Result<File, PlatformError> {
        let name = validate_single_component(name)?;
        let path = self.path.join(name);
        let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let descriptor = rfs::openat(&self.directory, name, flags, Mode::empty())
            .map_err(|errno| map_file_open_error(&path, errno))?;
        self.finish_state_file(descriptor, path, require_single_link)
    }

    fn finish_state_file(
        &self,
        descriptor: OwnedFd,
        path: PathBuf,
        require_single_link: bool,
    ) -> Result<File, PlatformError> {
        let stat = fstat(&descriptor, &path)?;
        verify_file(&stat, &path)?;
        check_acl(descriptor.as_fd(), &path, super::AclPolicy::OwnerOnly)?;
        rfs::fcntl_setfl(&descriptor, OFlags::empty())
            .map_err(|errno| PlatformError::io(&path, errno))?;
        if require_single_link
            && rfs::fstat(&descriptor)
                .map_err(|errno| PlatformError::io(&path, errno))?
                .st_nlink
                != 1
        {
            return Err(PlatformError::AclViolation {
                path,
                detail: "file con più collegamenti".into(),
            });
        }
        Ok(File::from(descriptor))
    }

    /// Pubblica `temp` come `target` soltanto se `target` non esiste.
    ///
    /// Usa `renameat2(RENAME_NOREPLACE)` su Linux e `renameatx_np(RENAME_EXCL)`
    /// su macOS, riferiti al descrittore ancorato: la comparsa di `target` fra
    /// la classificazione e la pubblicazione produce
    /// [`PlatformError::AlreadyExists`] e nessuna sovrascrittura. Un file
    /// system che non offre la primitiva produce un errore esplicito.
    pub fn publish_file(&self, temp: &OsStr, target: &OsStr) -> Result<(), PlatformError> {
        let temp = validate_single_component(temp)?;
        let target = validate_single_component(target)?;
        rfs::renameat_with(
            &self.directory,
            temp,
            &self.directory,
            target,
            RenameFlags::NOREPLACE,
        )
        .map_err(|errno| {
            if errno == Errno::EXIST {
                PlatformError::AlreadyExists {
                    path: self.path.join(target),
                }
            } else {
                rename_error(self, temp, target, errno)
            }
        })
    }

    /// Sostituisce atomicamente `target` esistente con `temp`.
    ///
    /// `renameat` rispetto alla directory ancorata: il file di destinazione
    /// esiste prima e dopo la chiamata e un lettore vede sempre una
    /// generazione completa. Se il rinominato fallisce, la destinazione resta
    /// invariata.
    pub fn replace_file(&self, temp: &OsStr, target: &OsStr) -> Result<(), PlatformError> {
        let temp = validate_single_component(temp)?;
        let target = validate_single_component(target)?;
        rfs::renameat(&self.directory, temp, &self.directory, target)
            .map_err(|errno| rename_error(self, temp, target, errno))
    }

    /// Rimuove un file nella radice ancorata, senza seguire collegamenti.
    ///
    /// Un nome assente produce [`PlatformError::FileAbsent`]; una directory con
    /// quel nome produce un errore di sistema e non viene rimossa.
    pub fn remove_file(&self, name: &OsStr) -> Result<(), PlatformError> {
        let name = validate_single_component(name)?;
        rfs::unlinkat(&self.directory, name, AtFlags::empty()).map_err(|errno| {
            if errno == Errno::NOENT {
                PlatformError::FileAbsent {
                    path: self.path.join(name),
                }
            } else {
                PlatformError::io(self.path.join(name), errno)
            }
        })
    }

    /// Elenca i nomi delle voci della radice ancorata, senza seguirle.
    ///
    /// I nomi sono quelli restituiti da `readdir`; il chiamante decide quali
    /// oggetti aprirne o rimuoverne e con quali verifiche.
    pub fn list_file_names(&self) -> Result<Vec<OsString>, PlatformError> {
        let directory = rfs::Dir::read_from(&self.directory)
            .map_err(|errno| PlatformError::io(&self.path, errno))?;
        let mut names = Vec::new();
        for entry in directory {
            let entry = entry.map_err(|errno| PlatformError::io(&self.path, errno))?;
            names.push(OsStr::from_bytes(entry.file_name().to_bytes()).to_os_string());
        }
        Ok(names)
    }

    /// Metadati di una voce, letti senza seguirla.
    ///
    /// `None` è l'assenza osservata sotto la directory ancorata; un oggetto
    /// presente viene classificato per tipo, proprietario e modalità, senza
    /// aprirlo e senza ripararlo.
    pub(crate) fn entry_info(&self, name: &OsStr) -> Result<Option<EntryInfo>, PlatformError> {
        let name = validate_single_component(name)?;
        match rfs::statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                let stat = EntryStat::from(&stat);
                let kind = if stat.is_directory() {
                    EntryKind::Directory
                } else if stat.is_regular_file() {
                    EntryKind::Regular
                } else if stat.is_symlink() {
                    EntryKind::Symlink
                } else {
                    EntryKind::Other
                };
                Ok(Some(EntryInfo {
                    kind,
                    mode: stat.mode,
                    uid: stat.uid,
                }))
            }
            Err(errno) if errno == Errno::NOENT => Ok(None),
            Err(errno) => Err(PlatformError::io(self.path.join(name), errno)),
        }
    }

    /// Crea un collegamento simbolico senza sostituire la destinazione.
    ///
    /// Il nome deve essere un singolo componente; il contenuto del collegamento
    /// è il testo indicato, mai una destinazione seguita o risolta.
    pub(crate) fn create_symlink(&self, name: &OsStr, target: &OsStr) -> Result<(), PlatformError> {
        let name = validate_single_component(name)?;
        if target.is_empty() || target.as_bytes().contains(&0) {
            return Err(PlatformError::invalid(
                Path::new(target),
                "destinazione del collegamento vuota o con NUL",
            ));
        }
        rfs::symlinkat(target, &self.directory, name).map_err(|errno| {
            if errno == Errno::EXIST {
                PlatformError::AlreadyExists {
                    path: self.path.join(name),
                }
            } else {
                PlatformError::io(self.path.join(name), errno)
            }
        })
    }

    /// Legge il testo di un collegamento simbolico senza seguirlo.
    ///
    /// `None` è l'assenza; un oggetto presente che non è un collegamento
    /// produce [`PlatformError::NotASymlink`].
    pub(crate) fn read_symlink_target(
        &self,
        name: &OsStr,
    ) -> Result<Option<PathBuf>, PlatformError> {
        let name = validate_single_component(name)?;
        let path = self.path.join(name);
        match self.entry_info(name)? {
            None => return Ok(None),
            Some(info) if info.kind == EntryKind::Symlink => {}
            Some(_) => return Err(PlatformError::NotASymlink { path }),
        }
        match rfs::readlinkat(&self.directory, name, Vec::new()) {
            Ok(target) => Ok(Some(PathBuf::from(OsString::from_vec(target.into_bytes())))),
            Err(errno) if errno == Errno::INVAL => Err(PlatformError::NotASymlink { path }),
            Err(errno) if errno == Errno::NOENT => Err(PlatformError::FileAbsent { path }),
            Err(errno) => Err(PlatformError::io(path, errno)),
        }
    }

    /// Rimuove un collegamento simbolico verificato, senza seguirlo.
    pub(crate) fn remove_symlink(&self, name: &OsStr) -> Result<(), PlatformError> {
        let name = validate_single_component(name)?;
        let path = self.path.join(name);
        match self.entry_info(name)? {
            None => {
                return Err(PlatformError::FileAbsent { path });
            }
            Some(info) if info.kind == EntryKind::Symlink && info.uid == super::euid() => {}
            Some(_) => return Err(PlatformError::NotASymlink { path }),
        }
        self.remove_file(name)
    }

    /// Verifica o crea una sottodirectory con modalità 0700 se mancante.
    ///
    /// Una sottodirectory preesistente non viene riparata: deve appartenere
    /// all'utente, essere una directory reale e non essere scrivibile da gruppo
    /// o altri. Un collegamento simbolico è rifiutato.
    pub(crate) fn ensure_subdirectory(&self, name: &OsStr) -> Result<VerifiedRoot, PlatformError> {
        let name = validate_single_component(name)?;
        let path = self.path.join(name);
        let created = match rfs::mkdirat(&self.directory, name, Mode::RWXU) {
            Ok(()) => true,
            Err(errno) if errno == Errno::EXIST => false,
            Err(errno) => return Err(PlatformError::io(&path, errno)),
        };

        let directory = rfs::openat(
            &self.directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|errno| map_open_error(&path, errno))?;
        if created {
            // L'umask può solo togliere bit: la modalità viene imposta.
            rfs::fchmod(&directory, Mode::RWXU).map_err(|errno| PlatformError::io(&path, errno))?;
        }

        let stat = fstat(&directory, &path)?;
        verify_containing(&stat, &path)?;
        check_acl(
            directory.as_fd(),
            &path,
            super::AclPolicy::DirectoryReplacement,
        )?;

        let named = rfs::statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|errno| PlatformError::io(&path, errno))?;
        let named = EntryStat::from(&named);
        if named.device != stat.device || named.inode != stat.inode {
            return Err(PlatformError::PathChanged { path });
        }

        Ok(VerifiedRoot {
            directory,
            path,
            device: stat.device,
            inode: stat.inode,
        })
    }

    /// Apre una sottodirectory esistente in sola lettura, senza crearla.
    pub(crate) fn open_subdirectory(
        &self,
        name: &OsStr,
    ) -> Result<Option<VerifiedRoot>, PlatformError> {
        let name = validate_single_component(name)?;
        let path = self.path.join(name);
        let directory = match rfs::openat(
            &self.directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(directory) => directory,
            Err(errno) if errno == Errno::NOENT => return Ok(None),
            Err(errno) => return Err(map_open_error(&path, errno)),
        };

        let stat = fstat(&directory, &path)?;
        verify_containing(&stat, &path)?;
        check_acl(
            directory.as_fd(),
            &path,
            super::AclPolicy::DirectoryReplacement,
        )?;

        Ok(Some(VerifiedRoot {
            directory,
            path,
            device: stat.device,
            inode: stat.inode,
        }))
    }

    /// Sincronizza la voce di directory della radice ancorata.
    pub fn sync_directory(&self) -> Result<super::DirectorySync, PlatformError> {
        rfs::fsync(&self.directory).map_err(|errno| PlatformError::io(&self.path, errno))?;
        Ok(super::DirectorySync::Synced)
    }
}

/// Sincronizza un file già scritto: `F_FULLFSYNC` su macOS, `fsync` su Linux.
pub(super) fn sync_file(file: &File, path: &Path) -> Result<(), PlatformError> {
    #[cfg(target_os = "macos")]
    {
        rfs::fcntl_fullfsync(file).map_err(|errno| PlatformError::io(path, errno))
    }
    #[cfg(not(target_os = "macos"))]
    {
        rfs::fsync(file).map_err(|errno| PlatformError::io(path, errno))
    }
}

/// Errore di rinomina rispetto alla directory ancorata: un temporaneo assente è
/// un'assenza del file richiesto, ogni altro errore resta di sistema.
fn rename_error(root: &VerifiedRoot, temp: &OsStr, target: &OsStr, errno: Errno) -> PlatformError {
    if errno == Errno::NOENT {
        PlatformError::FileAbsent {
            path: root.path.join(temp),
        }
    } else {
        PlatformError::io(root.path.join(target), errno)
    }
}

#[derive(Clone, Copy)]
struct EntryStat {
    kind: u32,
    mode: u32,
    uid: u32,
    device: u64,
    inode: u64,
}

impl EntryStat {
    #[allow(clippy::unnecessary_cast)]
    fn from(stat: &rfs::Stat) -> Self {
        Self {
            kind: stat.st_mode as u32 & TYPE_MASK,
            mode: stat.st_mode as u32 & 0o7777,
            uid: stat.st_uid as u32,
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
        }
    }

    fn is_directory(&self) -> bool {
        self.kind == TYPE_DIRECTORY
    }

    fn is_regular_file(&self) -> bool {
        self.kind == TYPE_REGULAR
    }

    fn is_symlink(&self) -> bool {
        self.kind == TYPE_SYMLINK
    }
}

/// Separa il prefisso esistente dal suffisso di componenti mancanti.
///
/// I collegamenti simbolici degli antenati esistenti vengono risolti; un
/// collegamento sull'ultimo componente, cioè sulla radice, è rifiutato.
fn walk_existing(nominal: &Path) -> Result<(PathBuf, Vec<OsString>), PlatformError> {
    let components: Vec<Component<'_>> = nominal.components().collect();
    let mut current = PathBuf::new();

    for (index, component) in components.iter().enumerate() {
        match component {
            Component::RootDir | Component::Prefix(_) => current.push(component.as_os_str()),
            Component::Normal(name) => {
                current.push(name);
                match std::fs::symlink_metadata(&current) {
                    Ok(metadata) => {
                        let last = index + 1 == components.len();
                        if metadata.file_type().is_symlink() {
                            if last {
                                return Err(PlatformError::SymlinkRefused { path: current });
                            }
                            current = current
                                .canonicalize()
                                .map_err(|error| PlatformError::io(&current, error))?;
                        } else if !metadata.is_dir() {
                            return Err(PlatformError::NotADirectory { path: current });
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        let parent = current
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_else(|| PathBuf::from("/"));
                        let missing = components[index..]
                            .iter()
                            .filter_map(|component| match component {
                                Component::Normal(name) => Some(name.to_os_string()),
                                _ => None,
                            })
                            .collect();
                        return Ok((parent, missing));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotADirectory => {
                        return Err(PlatformError::NotADirectory { path: current });
                    }
                    Err(error) => return Err(PlatformError::io(&current, error)),
                }
            }
            Component::CurDir | Component::ParentDir => {
                return Err(PlatformError::invalid(
                    nominal,
                    "componente non ammesso nel percorso",
                ));
            }
        }
    }

    Ok((current, Vec::new()))
}

/// Verifica la catena fisica esistente dalla radice del filesystem al prefisso.
fn verify_ancestors(base: &Path, first_missing: Option<&OsStr>) -> Result<(), PlatformError> {
    let paths = ancestor_paths(base);
    for (index, path) in paths.iter().enumerate() {
        let directory = open_directory(path)?;
        let stat = fstat(&directory, path)?;
        let child = paths
            .get(index + 1)
            .and_then(|path| path.file_name())
            .or(first_missing);
        verify_ancestor(&directory, path, &stat, child)?;
    }
    Ok(())
}

fn ancestor_paths(base: &Path) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("/")];
    let mut current = PathBuf::from("/");
    for component in base.components() {
        if let Component::Normal(name) = component {
            current.push(name);
            paths.push(current.clone());
        }
    }
    paths
}

fn verify_ancestor(
    directory: &OwnedFd,
    path: &Path,
    stat: &EntryStat,
    child: Option<&OsStr>,
) -> Result<(), PlatformError> {
    if !stat.is_directory() {
        return Err(PlatformError::NotADirectory {
            path: path.to_path_buf(),
        });
    }

    let owner = super::euid();
    if stat.uid != owner && stat.uid != 0 {
        return Err(PlatformError::WrongOwner {
            path: path.to_path_buf(),
            expected: format!("{owner} o 0"),
            actual: stat.uid.to_string(),
        });
    }

    if stat.mode & GROUP_OR_OTHER_WRITE != 0 {
        if stat.mode & STICKY == 0 {
            return Err(PlatformError::AclViolation {
                path: path.to_path_buf(),
                detail: "directory scrivibile da gruppo o altri senza sticky bit".to_owned(),
            });
        }
        verify_sticky_child(directory, path, child)?;
    }

    check_acl(
        directory.as_fd(),
        path,
        super::AclPolicy::DirectoryReplacement,
    )
}

/// Eccezione per directory con sticky bit: la voce sottostante deve appartenere
/// all'utente, così nessun altro account può rinominarla o cancellarla. Una
/// voce assente verrà creata dall'utente.
fn verify_sticky_child(
    directory: &OwnedFd,
    path: &Path,
    child: Option<&OsStr>,
) -> Result<(), PlatformError> {
    let Some(name) = child else {
        return Err(PlatformError::AclViolation {
            path: path.to_path_buf(),
            detail:
                "directory scrivibile da altri senza una voce sottostante posseduta dall'utente"
                    .to_owned(),
        });
    };

    match rfs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            let stat = EntryStat::from(&stat);
            if stat.uid != super::euid() {
                return Err(PlatformError::AclViolation {
                    path: path.join(name),
                    detail: "voce protetta solo dallo sticky bit ma posseduta da un altro account"
                        .to_owned(),
                });
            }
            Ok(())
        }
        Err(errno) if errno == Errno::NOENT => Ok(()),
        Err(errno) => Err(PlatformError::io(path.join(name), errno)),
    }
}

/// Crea un componente mancante con modalità 0700 imposta, oppure verifica un
/// oggetto comparso nel frattempo come preesistente.
fn enter_child_directory(
    parent: &OwnedFd,
    parent_path: &Path,
    name: &OsStr,
) -> Result<(OwnedFd, PathBuf), PlatformError> {
    let path = parent_path.join(name);
    match rfs::mkdirat(parent, name, Mode::RWXU) {
        Ok(()) => {
            rfs::chmodat(parent, name, Mode::RWXU, AtFlags::empty())
                .map_err(|errno| PlatformError::io(&path, errno))?;
        }
        Err(errno) if errno == Errno::EXIST => {}
        Err(errno) => return Err(PlatformError::io(&path, errno)),
    }

    let directory = rfs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| map_open_error(&path, errno))?;

    let stat = fstat(&directory, &path)?;
    verify_root(&stat, &path)?;
    check_acl(directory.as_fd(), &path, super::AclPolicy::OwnerOnly)?;

    // Il nome deve ancora indicare la directory appena aperta.
    let named = rfs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|errno| PlatformError::io(&path, errno))?;
    let named = EntryStat::from(&named);
    if named.device != stat.device || named.inode != stat.inode {
        return Err(PlatformError::PathChanged { path });
    }

    Ok((directory, path))
}

fn verify_root(stat: &EntryStat, path: &Path) -> Result<(), PlatformError> {
    if !stat.is_directory() {
        return Err(PlatformError::NotADirectory {
            path: path.to_path_buf(),
        });
    }
    let owner = super::euid();
    if stat.uid != owner {
        return Err(PlatformError::WrongOwner {
            path: path.to_path_buf(),
            expected: owner.to_string(),
            actual: stat.uid.to_string(),
        });
    }
    if stat.mode != DIR_MODE {
        return Err(PlatformError::WrongMode {
            path: path.to_path_buf(),
            expected: DIR_MODE,
            actual: stat.mode,
        });
    }
    Ok(())
}

/// Verifica una directory preesistente destinata a contenere oggetti
/// dell'utente: proprietario l'utente, nessun diritto di scrittura a gruppo o
/// altri. La modalità esatta non viene imposta e non viene modificata.
fn verify_containing(stat: &EntryStat, path: &Path) -> Result<(), PlatformError> {
    if !stat.is_directory() {
        return Err(PlatformError::NotADirectory {
            path: path.to_path_buf(),
        });
    }
    let owner = super::euid();
    if stat.uid != owner {
        return Err(PlatformError::WrongOwner {
            path: path.to_path_buf(),
            expected: owner.to_string(),
            actual: stat.uid.to_string(),
        });
    }
    if stat.mode & GROUP_OR_OTHER_WRITE != 0 {
        return Err(PlatformError::AclViolation {
            path: path.to_path_buf(),
            detail: format!(
                "directory scrivibile da gruppo o altri (modalità {:04o})",
                stat.mode
            ),
        });
    }
    Ok(())
}

fn verify_file(stat: &EntryStat, path: &Path) -> Result<(), PlatformError> {
    if !stat.is_regular_file() {
        return Err(PlatformError::NotARegularFile {
            path: path.to_path_buf(),
        });
    }
    let owner = super::euid();
    if stat.uid != owner {
        return Err(PlatformError::WrongOwner {
            path: path.to_path_buf(),
            expected: owner.to_string(),
            actual: stat.uid.to_string(),
        });
    }
    if stat.mode != FILE_MODE {
        return Err(PlatformError::WrongMode {
            path: path.to_path_buf(),
            expected: FILE_MODE,
            actual: stat.mode,
        });
    }
    Ok(())
}

fn open_directory(path: &Path) -> Result<OwnedFd, PlatformError> {
    rfs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| map_open_error(path, errno))
}

fn fstat(directory: &OwnedFd, path: &Path) -> Result<EntryStat, PlatformError> {
    rfs::fstat(directory)
        .map(|stat| EntryStat::from(&stat))
        .map_err(|errno| PlatformError::io(path, errno))
}

fn map_open_error(path: &Path, errno: Errno) -> PlatformError {
    if errno == Errno::LOOP {
        PlatformError::SymlinkRefused {
            path: path.to_path_buf(),
        }
    } else if errno == Errno::NOTDIR {
        PlatformError::NotADirectory {
            path: path.to_path_buf(),
        }
    } else {
        PlatformError::io(path, errno)
    }
}

/// Errore di apertura di un file esistente sotto la radice ancorata.
///
/// `ENOENT` è l'assenza del file richiesto rispetto al descrittore verificato:
/// non è un cambio di percorso. Il resto della classificazione resta comune:
/// un collegamento è `SymlinkRefused`, un componente non directory è
/// `NotADirectory`, ogni altro errore, compreso l'accesso negato, è `Io`.
fn map_file_open_error(path: &Path, errno: Errno) -> PlatformError {
    if errno == Errno::NOENT {
        PlatformError::FileAbsent {
            path: path.to_path_buf(),
        }
    } else {
        map_open_error(path, errno)
    }
}

/// Errore di apertura del file di lock preesistente.
///
/// Una directory aperta in lettura e scrittura produce `EISDIR`: è un tipo non
/// regolare, non un errore di sistema generico. Il resto della classificazione
/// resta quello dei file di stato: collegamento rifiutato, assenza dedicata,
/// accesso negato come errore di sistema.
fn lock_open_error(path: &Path, errno: Errno) -> PlatformError {
    if errno == Errno::ISDIR {
        PlatformError::NotARegularFile {
            path: path.to_path_buf(),
        }
    } else {
        map_file_open_error(path, errno)
    }
}

fn check_acl(
    descriptor: std::os::fd::BorrowedFd<'_>,
    path: &Path,
    policy: super::AclPolicy,
) -> Result<(), PlatformError> {
    #[cfg(target_os = "macos")]
    {
        macos_acl::check(descriptor, path, policy)
    }
    #[cfg(target_os = "linux")]
    {
        linux_acl::check(descriptor, path, policy)
    }
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
    {
        let _ = (descriptor, policy);
        Err(PlatformError::UnverifiableProtection {
            path: path.to_path_buf(),
            detail: "lettura ACL non disponibile su questa piattaforma".to_owned(),
        })
    }
}
