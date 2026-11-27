//! Prove d'integrazione del modulo `platform` sui sistemi Unix.
//!
//! Ogni prova usa una directory temporanea sotto `target/platform-tests`: nessun
//! profilo, permesso o configurazione reale del computer viene modificato. Le
//! prove con un secondo account e su Windows non sono eseguite qui.

#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rustix::fs::Mode;
use totum_daemon::platform::{self, PlatformError, VerifiedRoot};

static NEXT: AtomicU32 = AtomicU32::new(0);
// L'umask è globale per il processo: le prove che lo cambiano vanno serializzate
// rispetto a ogni altra prova che crea file o directory.
static SUITE_LOCK: Mutex<()> = Mutex::new(());

struct Scratch {
    path: PathBuf,
    _lock: MutexGuard<'static, ()>,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let lock = SUITE_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("radice del workspace")
            .join("target/platform-tests");
        fs::create_dir_all(&base).expect("directory base creabile");
        let path = base.join(format!(
            "{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("directory di prova creabile");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("permessi della directory di prova");
        Scratch { path, _lock: lock }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Imposta l'umask per la durata del guard e serializza le prove che lo usano.
struct Umask {
    previous: Mode,
}

impl Umask {
    fn set(mask: Mode) -> Self {
        let previous = rustix::process::umask(mask);
        Umask { previous }
    }
}

impl Drop for Umask {
    fn drop(&mut self) {
        rustix::process::umask(self.previous);
    }
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("metadati disponibili")
        .permissions()
        .mode()
        & 0o7777
}

fn inode(path: &Path) -> u64 {
    fs::symlink_metadata(path)
        .expect("metadati disponibili")
        .ino()
}

fn prepare(path: &Path) -> Result<VerifiedRoot, PlatformError> {
    platform::prepare_root(path)
}

#[test]
fn creates_missing_components_with_forced_permissions() {
    let scratch = Scratch::new("umask-000");
    let nominal = scratch.path.join("a/b/totum");

    let umask = Umask::set(Mode::from_bits_retain(0o000));
    let root = prepare(&nominal).expect("radice preparabile con umask 000");
    let file = root
        .create_file(OsStr::new("config.json"))
        .expect("file creabile");
    drop(file);
    drop(umask);

    for directory in [
        scratch.path.join("a"),
        scratch.path.join("a/b"),
        nominal.clone(),
    ] {
        assert_eq!(mode(&directory), 0o700, "{}", directory.display());
        assert_eq!(
            fs::metadata(&directory).unwrap().uid(),
            rustix::process::geteuid().as_raw()
        );
    }
    assert_eq!(mode(&nominal.join("config.json")), 0o600);
}

#[test]
fn permissive_umask_cannot_weaken_created_objects() {
    let scratch = Scratch::new("umask-777");
    let nominal = scratch.path.join("totum");

    let umask = Umask::set(Mode::from_bits_retain(0o777));
    let root = prepare(&nominal).expect("radice preparabile con umask 777");
    let file = root
        .create_file(OsStr::new("state"))
        .expect("file creabile");
    drop(file);
    drop(umask);

    assert_eq!(mode(&nominal), 0o700);
    assert_eq!(mode(&nominal.join("state")), 0o600);
}

#[test]
fn interrupted_initialization_is_verified_and_reused() {
    let scratch = Scratch::new("interrupted");
    let prefix = scratch.path.join("a/b");
    fs::create_dir_all(&prefix).expect("prefisso creato a mano");
    for directory in [scratch.path.join("a"), prefix.clone()] {
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let before = inode(&prefix);
    let nominal = prefix.join("totum");

    let first = prepare(&nominal).expect("prefisso parziale riusabile");
    let root_inode = inode(&nominal);
    drop(first);

    let second = prepare(&nominal).expect("seconda preparazione riuscita");
    assert_eq!(inode(&prefix), before, "il prefisso non viene ricreato");
    assert_eq!(inode(&nominal), root_inode, "la radice viene riusata");
    assert_eq!(mode(&nominal), 0o700);
    drop(second);
}

#[test]
fn existing_root_with_wrong_mode_is_refused_and_unchanged() {
    let scratch = Scratch::new("wrong-mode");
    let nominal = scratch.path.join("totum");
    fs::create_dir(&nominal).unwrap();
    fs::set_permissions(&nominal, fs::Permissions::from_mode(0o755)).unwrap();

    let error = prepare(&nominal).expect_err("modalità 0755 rifiutata");
    match error {
        PlatformError::WrongMode {
            expected, actual, ..
        } => {
            assert_eq!(expected, 0o700);
            assert_eq!(actual, 0o755);
        }
        other => panic!("errore inatteso: {other}"),
    }
    assert_eq!(mode(&nominal), 0o755, "la modalità non viene riparata");
}

#[test]
fn existing_root_of_wrong_type_is_refused() {
    let scratch = Scratch::new("root-file");
    let nominal = scratch.path.join("totum");
    fs::write(&nominal, b"contenuto").unwrap();

    assert!(matches!(
        prepare(&nominal),
        Err(PlatformError::NotADirectory { .. })
    ));
    assert_eq!(fs::read(&nominal).unwrap(), b"contenuto");
}

#[test]
fn symlink_root_is_refused() {
    let scratch = Scratch::new("root-symlink");
    let target = scratch.path.join("target");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    let nominal = scratch.path.join("totum");
    symlink(&target, &nominal).unwrap();

    assert!(matches!(
        prepare(&nominal),
        Err(PlatformError::SymlinkRefused { .. })
    ));
    assert!(fs::read_dir(&target).unwrap().next().is_none());
}

#[test]
fn symlinked_ancestors_are_resolved() {
    let scratch = Scratch::new("ancestor-symlink");
    let real = scratch.path.join("real");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    let link = scratch.path.join("link");
    symlink(&real, &link).unwrap();

    let root = prepare(&link.join("totum")).expect("antenato risolto");
    let expected = real.canonicalize().unwrap().join("totum");
    assert_eq!(root.path(), expected);
    assert_eq!(mode(&expected), 0o700);
}

#[test]
fn creates_and_reopens_files() {
    let scratch = Scratch::new("file-roundtrip");
    let root = prepare(&scratch.path.join("totum")).expect("radice preparabile");

    let mut file = root
        .create_file(OsStr::new("config.json"))
        .expect("file creabile");
    file.write_all(b"{\"revision\":0}").unwrap();
    drop(file);
    assert_eq!(mode(&root.path().join("config.json")), 0o600);

    let mut reopened = root
        .open_file(OsStr::new("config.json"))
        .expect("file riapribile");
    let mut content = String::new();
    reopened.read_to_string(&mut content).unwrap();
    assert_eq!(content, "{\"revision\":0}");
}

#[test]
fn missing_file_is_reported_as_absent_not_as_a_changed_root() {
    let scratch = Scratch::new("file-absent");
    let root = prepare(&scratch.path.join("totum")).expect("radice preparabile");

    match root.open_file(OsStr::new("config.json")) {
        Err(PlatformError::FileAbsent { path }) => {
            assert_eq!(path, root.path().join("config.json"))
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    root.verify_path().expect("la radice non è cambiata");
}

#[test]
fn removed_root_is_reported_as_a_changed_path() {
    let scratch = Scratch::new("root-removed");
    let nominal = scratch.path.join("totum");
    let root = prepare(&nominal).expect("radice preparabile");

    fs::remove_dir(&nominal).expect("radice rimossa");
    assert!(matches!(
        root.verify_path(),
        Err(PlatformError::PathChanged { .. })
    ));
}

/// Variabile che fa eseguire alla stessa prova il ramo nel processo figlio.
const FIFO_CHILD_ENV: &str = "TOTUM_PLATFORM_FIFO_CHILD";
const FIFO_CHILD_TIMEOUT: Duration = Duration::from_secs(15);

fn fifo_child_opens_without_writer(base: &Path) {
    let root = prepare(&base.join("totum")).expect("radice preparabile nel processo figlio");
    let fifo = root.path().join("pipe");
    let created = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !created {
        eprintln!("mkfifo non disponibile: prova saltata");
        return;
    }

    let error = root
        .open_file(OsStr::new("pipe"))
        .expect_err("una FIFO non è un file regolare");
    assert!(
        matches!(error, PlatformError::NotARegularFile { .. }),
        "errore inatteso: {error:?}"
    );
}

#[test]
fn fifo_without_writer_is_rejected_without_blocking() {
    if let Some(base) = std::env::var_os(FIFO_CHILD_ENV) {
        fifo_child_opens_without_writer(Path::new(&base));
        return;
    }

    // Il caso difettoso blocca l'apertura: la prova gira in un processo figlio
    // con un termine massimo, così un blocco non blocca l'intera suite.
    let scratch = Scratch::new("fifo");
    let executable = std::env::current_exe().expect("percorso dell'eseguibile di prova");
    let mut child = Command::new(executable)
        .args([
            "fifo_without_writer_is_rejected_without_blocking",
            "--exact",
            "--nocapture",
        ])
        .env(FIFO_CHILD_ENV, &scratch.path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("processo figlio avviabile");

    let deadline = Instant::now() + FIFO_CHILD_TIMEOUT;
    let status = loop {
        match child
            .try_wait()
            .expect("stato del processo figlio leggibile")
        {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "l'apertura della FIFO senza scrittore non è terminata entro {FIFO_CHILD_TIMEOUT:?}"
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };

    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        panic!("il processo figlio è fallito ({status}):\n{stderr}");
    }
}

#[test]
fn preexisting_objects_are_never_followed_or_repaired() {
    let scratch = Scratch::new("preexisting");
    let outside = scratch.path.join("outside.json");
    fs::write(&outside, b"segreti").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();

    let root = prepare(&scratch.path.join("totum")).expect("radice preparabile");
    symlink(&outside, root.path().join("config.json")).unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();

    assert!(matches!(
        root.create_file(OsStr::new("config.json")),
        Err(PlatformError::AlreadyExists { .. })
    ));
    assert!(matches!(
        root.open_file(OsStr::new("config.json")),
        Err(PlatformError::SymlinkRefused { .. })
    ));
    assert!(matches!(
        root.create_file(OsStr::new("sub")),
        Err(PlatformError::AlreadyExists { .. })
    ));
    assert!(matches!(
        root.open_file(OsStr::new("sub")),
        Err(PlatformError::NotARegularFile { .. })
    ));
    assert_eq!(fs::read(&outside).unwrap(), b"segreti");
    assert_eq!(mode(&outside), 0o600);
}

#[test]
fn open_file_refuses_weakened_permissions_without_repairing_them() {
    let scratch = Scratch::new("file-mode");
    let root = prepare(&scratch.path.join("totum")).expect("radice preparabile");
    let path = root.path().join("state");
    let file = root.create_file(OsStr::new("state")).unwrap();
    drop(file);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(matches!(
        root.open_file(OsStr::new("state")),
        Err(PlatformError::WrongMode {
            expected: 0o600,
            actual: 0o644,
            ..
        })
    ));
    assert_eq!(mode(&path), 0o644, "la modalità non viene riparata");
}

#[test]
fn renamed_root_keeps_operations_anchored_to_the_descriptor() {
    let scratch = Scratch::new("anchor");
    let nominal = scratch.path.join("totum");
    let root = prepare(&nominal).expect("radice preparabile");

    let moved = scratch.path.join("moved");
    fs::rename(&nominal, &moved).unwrap();
    fs::create_dir(&nominal).unwrap();
    fs::set_permissions(&nominal, fs::Permissions::from_mode(0o700)).unwrap();

    assert!(matches!(
        root.verify_path(),
        Err(PlatformError::PathChanged { .. })
    ));
    let file = root
        .create_file(OsStr::new("state"))
        .expect("l'ancoraggio scrive nella directory verificata");
    drop(file);
    assert!(
        moved.join("state").exists(),
        "scrittura nella radice ancorata"
    );
    assert!(
        !nominal.join("state").exists(),
        "nessuna scrittura nella directory sostituita"
    );
}

#[test]
fn shared_sticky_directory_is_accepted_for_an_entry_owned_by_the_user() {
    let shared = Path::new("/tmp");
    let Ok(metadata) = fs::metadata(shared) else {
        eprintln!("/tmp non disponibile: prova saltata");
        return;
    };
    if metadata.permissions().mode() & 0o1000 == 0 {
        eprintln!("/tmp senza sticky bit: prova saltata");
        return;
    }

    let nominal = shared.join(format!(
        "totum-platform-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let root = prepare(&nominal).expect("directory condivisa sticky accettata");
    assert_eq!(mode(&nominal), 0o700);
    drop(root);
    fs::remove_dir_all(&nominal).unwrap();
}

#[test]
fn state_root_is_absolute_and_named() {
    let root = platform::state_root().expect("radice nominale risolvibile");
    assert!(root.is_absolute());
    let name = root.file_name().expect("nome finale presente");
    #[cfg(target_os = "macos")]
    assert_eq!(name, "Totum");
    #[cfg(target_os = "linux")]
    assert_eq!(name, "totum");
}

#[cfg(target_os = "macos")]
mod macos_acl {
    use super::*;

    fn apply(path: &Path, acl: &str) -> bool {
        Command::new("chmod")
            .args(["+a", acl, path.to_str().expect("percorso UTF-8")])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn clear(path: &Path) {
        let _ = Command::new("chmod")
            .args(["-N", path.to_str().expect("percorso UTF-8")])
            .status();
    }

    #[test]
    fn ancestor_acl_allowing_replacement_is_refused() {
        let scratch = Scratch::new("acl-ancestor");
        let parent = scratch.path.join("parent");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        if !apply(&parent, "everyone allow write") {
            eprintln!("chmod +a non disponibile: prova saltata");
            return;
        }
        let nominal = parent.join("totum");

        let result = prepare(&nominal);
        clear(&parent);
        assert!(
            matches!(result, Err(PlatformError::AclViolation { .. })),
            "un ACL che consente la sostituzione va rifiutato"
        );
        assert!(!nominal.exists(), "nessuna scrittura dopo il rifiuto");
    }

    #[test]
    fn root_acl_allowing_other_users_is_refused() {
        let scratch = Scratch::new("acl-root");
        let nominal = scratch.path.join("totum");
        fs::create_dir(&nominal).unwrap();
        fs::set_permissions(&nominal, fs::Permissions::from_mode(0o700)).unwrap();
        if !apply(&nominal, "everyone allow read") {
            eprintln!("chmod +a non disponibile: prova saltata");
            return;
        }

        let result = prepare(&nominal);
        clear(&nominal);
        assert!(matches!(result, Err(PlatformError::AclViolation { .. })));
        assert_eq!(mode(&nominal), 0o700, "la modalità resta invariata");
    }

    #[test]
    fn inherited_acl_on_a_created_root_is_refused() {
        let scratch = Scratch::new("acl-inherited");
        let parent = scratch.path.join("parent");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        if !apply(
            &parent,
            "everyone allow read,file_inherit,directory_inherit",
        ) {
            eprintln!("chmod +a non disponibile: prova saltata");
            return;
        }
        let nominal = parent.join("totum");

        let result = prepare(&nominal);
        clear(&parent);
        assert!(
            matches!(result, Err(PlatformError::AclViolation { .. })),
            "un ACL ereditato che concede accesso va rifiutato"
        );
        assert!(
            !nominal.join("config.json").exists(),
            "nessuno stato applicativo dopo il rifiuto"
        );
        if nominal.exists() {
            clear(&nominal);
        }
    }

    #[test]
    fn deny_entries_do_not_block_preparation() {
        let scratch = Scratch::new("acl-deny");
        let nominal = scratch.path.join("totum");
        fs::create_dir(&nominal).unwrap();
        fs::set_permissions(&nominal, fs::Permissions::from_mode(0o700)).unwrap();
        if !apply(&nominal, "everyone deny delete") {
            eprintln!("chmod +a non disponibile: prova saltata");
            return;
        }

        let result = prepare(&nominal);
        clear(&nominal);
        assert!(result.is_ok(), "un ACL di diniego non è una violazione");
    }
}

#[cfg(target_os = "linux")]
mod linux_acl {
    use super::*;

    fn apply(path: &Path, spec: &str) -> bool {
        Command::new("setfacl")
            .args(["-m", spec])
            .arg(path)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn clear(path: &Path) {
        let _ = Command::new("setfacl").args(["-b"]).arg(path).status();
    }

    #[test]
    fn ancestor_acl_with_read_and_traverse_is_accepted() {
        let scratch = Scratch::new("posix-ancestor-rx");
        let ancestor = scratch.path.join("ancestor");
        fs::create_dir(&ancestor).unwrap();
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700)).unwrap();
        if !apply(&ancestor, "u:65534:r-x") {
            eprintln!("setfacl non disponibile: prova saltata");
            return;
        }

        let result = prepare(&ancestor.join("totum"));
        clear(&ancestor);
        assert!(
            result.is_ok(),
            "lettura e attraversamento per un altro account non sono sostituzione: {result:?}"
        );
    }

    #[test]
    fn ancestor_acl_with_write_is_refused() {
        let scratch = Scratch::new("posix-ancestor-w");
        let ancestor = scratch.path.join("ancestor");
        fs::create_dir(&ancestor).unwrap();
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700)).unwrap();
        if !apply(&ancestor, "u:65534:w") {
            eprintln!("setfacl non disponibile: prova saltata");
            return;
        }
        let nominal = ancestor.join("totum");

        let result = prepare(&nominal);
        clear(&ancestor);
        assert!(
            matches!(result, Err(PlatformError::AclViolation { .. })),
            "la scrittura per un altro account deve essere rifiutata"
        );
        assert!(!nominal.exists(), "nessuna scrittura dopo il rifiuto");
    }

    #[test]
    fn posix_acl_granting_access_is_refused() {
        let scratch = Scratch::new("posix-acl");
        let nominal = scratch.path.join("totum");
        fs::create_dir(&nominal).unwrap();
        fs::set_permissions(&nominal, fs::Permissions::from_mode(0o700)).unwrap();
        let applied = Command::new("setfacl")
            .args(["-m", "u:65534:rwx"])
            .arg(&nominal)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !applied {
            eprintln!("setfacl non disponibile: prova saltata");
            return;
        }

        let result = prepare(&nominal);
        let _ = Command::new("setfacl").args(["-b"]).arg(&nominal).status();
        assert!(matches!(result, Err(PlatformError::AclViolation { .. })));
    }
}
