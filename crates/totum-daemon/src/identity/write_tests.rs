//! Prove della creazione dei file iniziali e dei punti di guasto.
//!
//! Ogni prova usa una radice temporanea creata dalle primitive verificate di
//! `platform`; nessun profilo reale viene toccato. I guasti e gli arresti
//! simulati esistono soltanto nella compilazione dei test: il binario
//! operativo non ha percorsi diagnostici.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::write::{self, Step, create_temporary_with, fault};
use super::{CONTROLLERS_FILE, IDENTITY_FILE};
use crate::platform::{self, VerifiedRoot};

static NEXT: AtomicU32 = AtomicU32::new(0);

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("radice del workspace")
            .join("target/identity-tests");
        fs::create_dir_all(&base).expect("directory base creabile");
        let path = base.join(format!(
            "lib-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("directory di prova creabile");
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("permessi della directory di prova");
        Scratch { path }
    }

    fn root(&self) -> VerifiedRoot {
        platform::prepare_root(&self.path.join("totum")).expect("radice preparabile")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.path);
    }
}

const IDENTITY: &[u8] = b"{\"schemaVersion\":1,\"state\":\"notInitialized\"}\n";
const CONTROLLERS: &[u8] = b"{\"schemaVersion\":1,\"grantRevision\":0,\"controllers\":[]}\n";

fn write(root: &VerifiedRoot, name: &str, contents: &[u8]) {
    let mut file = root.create_file(OsStr::new(name)).expect("file creabile");
    file.write_all(contents).expect("contenuto scritto");
    file.sync_all().expect("contenuto sincronizzato");
}

fn read(directory: &Path, name: &str) -> Vec<u8> {
    fs::read(directory.join(name)).expect("file leggibile")
}

fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .expect("directory leggibile")
        .map(|entry| {
            entry
                .expect("voce leggibile")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn reserved_temporaries(directory: &Path) -> Vec<String> {
    fs::read_dir(directory)
        .expect("directory leggibile")
        .filter_map(|entry| {
            let name = entry.expect("voce leggibile").file_name();
            write::is_reserved_temporary(&name).then(|| name.to_string_lossy().into_owned())
        })
        .collect()
}

#[test]
fn create_missing_publishes_the_exact_document_without_temporaries() {
    let scratch = Scratch::new("publish");
    let root = scratch.root();
    let directory = root.path().to_path_buf();

    let created = write::create_missing(&root, IDENTITY_FILE, IDENTITY).expect("creato");
    assert_eq!(created.file_name(), IDENTITY_FILE);
    assert!(created.commit().durable());
    assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
    assert!(reserved_temporaries(&directory).is_empty());
    assert_eq!(entries(&directory), vec![IDENTITY_FILE.to_owned()]);

    #[cfg(unix)]
    {
        let mode = fs::metadata(directory.join(IDENTITY_FILE))
            .expect("metadati")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "il file pubblicato è del solo proprietario");
    }
}

#[test]
fn create_missing_never_replaces_an_existing_destination() {
    let scratch = Scratch::new("existing");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, IDENTITY_FILE, b"contenuto precedente");

    let error = write::create_missing(&root, IDENTITY_FILE, IDENTITY).expect_err("rifiutato");
    assert!(matches!(
        error,
        super::IdentityError::AppearedDuringInitialization { .. }
    ));
    assert_eq!(read(&directory, IDENTITY_FILE), b"contenuto precedente");
    assert!(reserved_temporaries(&directory).is_empty());
}

#[test]
fn a_destination_appearing_before_publication_is_not_overwritten() {
    let scratch = Scratch::new("appeared");
    let root = scratch.root();
    let directory = root.path().to_path_buf();

    let error = write::create_missing_with(&root, IDENTITY_FILE, IDENTITY, || {
        // Simula un altro scrittore che pubblica fra la classificazione e la
        // rinomina: la rinomina senza sostituzione deve perdere.
        let mut file = root
            .create_file(OsStr::new(IDENTITY_FILE))
            .expect("comparso");
        file.write_all(b"comparso prima della pubblicazione")
            .expect("scritto");
    })
    .expect_err("rifiutato");

    assert!(matches!(
        error,
        super::IdentityError::AppearedDuringInitialization { .. }
    ));
    assert_eq!(
        read(&directory, IDENTITY_FILE),
        b"comparso prima della pubblicazione"
    );
    assert!(reserved_temporaries(&directory).is_empty());
}

#[test]
fn failures_before_publication_leave_no_file_and_no_temporary() {
    for step in [
        Step::CreateTemp,
        Step::WriteTemp,
        Step::SyncTemp,
        Step::Publish,
    ] {
        let scratch = Scratch::new("fault");
        let root = scratch.root();
        let directory = root.path().to_path_buf();

        fault::arm(step);
        let result = write::create_missing(&root, IDENTITY_FILE, IDENTITY);
        fault::clear();

        assert!(result.is_err(), "{step:?}");
        assert!(
            !directory.join(IDENTITY_FILE).exists(),
            "{step:?}: nulla è pubblicato"
        );
        assert!(
            reserved_temporaries(&directory).is_empty(),
            "{step:?}: il temporaneo è stato rimosso"
        );
    }
}

#[test]
fn a_directory_sync_failure_after_publication_is_reported_as_not_durable() {
    let scratch = Scratch::new("not-durable");
    let root = scratch.root();
    let directory = root.path().to_path_buf();

    fault::arm(Step::SyncDirectory);
    let created = write::create_missing(&root, IDENTITY_FILE, IDENTITY).expect("pubblicato");
    fault::clear();

    assert!(!created.commit().durable());
    assert!(matches!(
        created.commit(),
        super::Commit::CommittedNotDurable { .. }
    ));
    assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
    assert!(reserved_temporaries(&directory).is_empty());
}

#[test]
fn temporary_name_collisions_are_bounded() {
    let scratch = Scratch::new("collision");
    let root = scratch.root();
    let name = OsString::from(".identity.1.0.tmp");
    write(&root, &name.to_string_lossy(), b"");

    let error = match create_temporary_with(&root, Step::CreateTemp, || name.clone()) {
        Ok(_) => panic!("nessun nome doveva essere disponibile"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        super::IdentityError::TemporaryCollision { attempts: 16 }
    ));
    assert_eq!(entries(root.path()), vec![".identity.1.0.tmp".to_owned()]);
}

#[test]
fn cleanup_only_touches_the_identity_namespace() {
    let scratch = Scratch::new("cleanup");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, ".identity.4242.0.tmp", b"residuo");
    write(&root, ".identity.7.31.tmp", b"residuo");
    write(&root, ".config.4242.0.tmp", b"configurazione");
    write(&root, "notes.txt", b"nota");
    write(&root, "identity.json.bak", b"copia");
    write(&root, "x.identity.1.0.tmp", b"estraneo");

    let removed = write::discard_temporaries(&root).expect("pulizia");
    assert_eq!(removed.len(), 2);
    assert!(reserved_temporaries(&directory).is_empty());
    assert_eq!(read(&directory, ".config.4242.0.tmp"), b"configurazione");
    assert_eq!(read(&directory, "notes.txt"), b"nota");
    assert_eq!(read(&directory, "identity.json.bak"), b"copia");
    assert_eq!(read(&directory, "x.identity.1.0.tmp"), b"estraneo");
}

// ---------------------------------------------------------------------------
// Arresto del processo fra i due file (processi figli)
// ---------------------------------------------------------------------------

const CRASH_ENV: &str = "TOTUM_TEST_IDENTITY_CRASH_SCENARIO";
const CRASH_DIR_ENV: &str = "TOTUM_TEST_IDENTITY_CRASH_DIR";
const CRASH_TEST: &str = "identity::write_tests::crash_child";

/// Processo figlio che termina senza unwinding al punto armato.
#[test]
#[ignore = "eseguito soltanto dai processi figli delle prove di arresto"]
fn crash_child() {
    let Ok(scenario) = std::env::var(CRASH_ENV) else {
        return;
    };
    if scenario == "fallback" {
        // Uscita ordinaria usata dalla prova del controllo del padre: non è un
        // arresto e non deve essere accettata.
        std::process::exit(17);
    }
    let base = PathBuf::from(std::env::var(CRASH_DIR_ENV).expect("directory del figlio"));
    let root = platform::prepare_root(&base.join("totum")).expect("radice del figlio preparabile");

    match scenario.as_str() {
        "before-identity-publish" => {
            fault::arm_crash(Step::Publish);
        }
        "after-identity-publish" => {
            fault::arm_crash(Step::SyncDirectory);
        }
        "before-controllers-publish" => {
            fault::arm_crash(Step::Publish);
        }
        "after-controllers-publish" => {
            fault::arm_crash(Step::SyncDirectory);
        }
        other => panic!("scenario di arresto sconosciuto: {other}"),
    }
    let mut locked = platform::lock(root).expect("lock di istanza acquisito");
    let _ = super::initialize(&mut locked);

    // Se il punto armato non ha arrestato il processo, il padre deve vedere
    // questa uscita ordinaria e rifiutarla, non accettarla come arresto.
    std::process::exit(17);
}

/// Riconosce lo specifico arresto di `std::process::abort`: un'uscita ordinaria,
/// un panico o il fallback `exit(17)` non lo soddisfano.
#[cfg(unix)]
fn is_expected_crash(status: &ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;

    status.signal() == Some(rustix::process::Signal::ABORT.as_raw()) && status.code().is_none()
}

/// Su Windows `std::process::abort` usa
/// `__fastfail(FAST_FAIL_FATAL_APP_EXIT)`, che termina con
/// `STATUS_STACK_BUFFER_OVERRUN`.
#[cfg(windows)]
fn is_expected_crash(status: &ExitStatus) -> bool {
    const STATUS_STACK_BUFFER_OVERRUN: i32 = 0xC000_0409u32 as i32;

    status.code() == Some(STATUS_STACK_BUFFER_OVERRUN)
}

fn run_child(scenario: &str, base: &Path) -> ExitStatus {
    let exe = std::env::current_exe().expect("binario di prova");
    let mut child = Command::new(exe)
        .args(["--ignored", "--exact", CRASH_TEST])
        .env(CRASH_ENV, scenario)
        .env(CRASH_DIR_ENV, base)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("processo figlio avviato");

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait().expect("stato del processo figlio") {
            Some(status) => return status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{scenario}: termine massimo superato");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn run_crash_child(scenario: &str, base: &Path) {
    let status = run_child(scenario, base);
    assert!(
        is_expected_crash(&status),
        "{scenario}: atteso l'arresto di std::process::abort, osservato {status:?}"
    );
}

#[test]
fn an_ordinary_exit_is_not_accepted_as_a_crash() {
    let scratch = Scratch::new("crash-fallback");
    let status = run_child("fallback", &scratch.path);
    assert!(!status.success());
    assert!(
        !is_expected_crash(&status),
        "un'uscita ordinaria non deve soddisfare il controllo: {status:?}"
    );
}

#[test]
fn a_crash_before_the_identity_publication_leaves_only_a_discardable_temporary() {
    let scratch = Scratch::new("crash-before-identity");
    run_crash_child("before-identity-publish", &scratch.path);
    let directory = scratch.path.join("totum");

    assert!(!directory.join(IDENTITY_FILE).exists());
    assert!(!directory.join(CONTROLLERS_FILE).exists());
    let temporaries = reserved_temporaries(&directory);
    assert_eq!(temporaries.len(), 1, "{temporaries:?}");
    assert_eq!(read(&directory, &temporaries[0]), IDENTITY);

    let mut locked = platform::lock(scratch.root()).expect("lock del padre");
    let startup = super::initialize(&mut locked).expect("riavvio completato");
    assert_eq!(startup.discarded_temporaries.len(), 1);
    assert_eq!(startup.created.len(), 2);
    assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
    assert_eq!(read(&directory, CONTROLLERS_FILE), CONTROLLERS);
    assert!(reserved_temporaries(&directory).is_empty());
}

#[test]
fn a_crash_after_the_identity_publication_is_completed_by_the_restart() {
    let scratch = Scratch::new("crash-after-identity");
    run_crash_child("after-identity-publish", &scratch.path);
    let directory = scratch.path.join("totum");

    assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
    assert!(!directory.join(CONTROLLERS_FILE).exists());
    assert!(reserved_temporaries(&directory).is_empty());

    let mut locked = platform::lock(scratch.root()).expect("lock del padre");
    let startup = super::initialize(&mut locked).expect("riavvio completato");
    assert_eq!(
        startup
            .created
            .iter()
            .map(super::Created::file_name)
            .collect::<Vec<_>>(),
        vec![CONTROLLERS_FILE],
        "il file già pubblicato non viene riscritto"
    );
    assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
    assert_eq!(read(&directory, CONTROLLERS_FILE), CONTROLLERS);
}

#[test]
fn a_crash_before_the_controllers_publication_is_completed_by_the_restart() {
    let scratch = Scratch::new("crash-before-controllers");
    let root = scratch.root();
    write(&root, IDENTITY_FILE, IDENTITY);
    drop(root);
    run_crash_child("before-controllers-publish", &scratch.path);
    let directory = scratch.path.join("totum");

    assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
    assert!(!directory.join(CONTROLLERS_FILE).exists());
    let temporaries = reserved_temporaries(&directory);
    assert_eq!(temporaries.len(), 1, "{temporaries:?}");
    assert_eq!(read(&directory, &temporaries[0]), CONTROLLERS);

    let mut locked = platform::lock(scratch.root()).expect("lock del padre");
    let startup = super::initialize(&mut locked).expect("riavvio completato");
    assert_eq!(
        startup
            .created
            .iter()
            .map(super::Created::file_name)
            .collect::<Vec<_>>(),
        vec![CONTROLLERS_FILE]
    );
    assert_eq!(read(&directory, CONTROLLERS_FILE), CONTROLLERS);
    assert!(reserved_temporaries(&directory).is_empty());
}

#[test]
fn a_crash_after_the_controllers_publication_leaves_a_complete_initial_state() {
    let scratch = Scratch::new("crash-after-controllers");
    let root = scratch.root();
    write(&root, IDENTITY_FILE, IDENTITY);
    drop(root);
    run_crash_child("after-controllers-publish", &scratch.path);
    let directory = scratch.path.join("totum");

    assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
    assert_eq!(read(&directory, CONTROLLERS_FILE), CONTROLLERS);
    assert!(reserved_temporaries(&directory).is_empty());

    let mut locked = platform::lock(scratch.root()).expect("lock del padre");
    let startup = super::initialize(&mut locked).expect("riavvio completato");
    assert!(startup.created.is_empty(), "nessuna riscrittura");
    assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
    assert_eq!(read(&directory, CONTROLLERS_FILE), CONTROLLERS);
}
