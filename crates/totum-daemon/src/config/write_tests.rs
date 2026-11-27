//! Prove della scrittura, del recupero e dei punti di guasto.
//!
//! Ogni prova usa una radice temporanea creata dalle primitive verificate di
//! `platform`; nessun profilo o configurazione reale viene toccata. I guasti
//! simulati sono attivi soltanto nella compilazione dei test e sono per thread:
//! non esiste alcun comando diagnostico nel binario operativo.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::document::{self, Parsed};
use super::write::{self, Step, create_temporary_with, fault, is_reserved_temporary};
use super::{
    CONFIG_BACKUP_FILE, CONFIG_FILE, CoherenceInput, Commit, ComponentState, ConfigError, ConfigV1,
    GenerationFailure, MigrationSource, StartupKind, Store,
};
use crate::platform::{self, VerifiedRoot};
use crate::test_support::{fork_window, spawn};

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
            .join("target/config-tests");
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
        let _ = fs::remove_dir_all(&self.path);
    }
}

const COHERENT: CoherenceInput = CoherenceInput {
    identity: ComponentState::Initial,
    registry: ComponentState::Initial,
};

fn valid(revision: u64) -> Vec<u8> {
    format!("{{\"schemaVersion\":1,\"revision\":{revision}}}").into_bytes()
}

fn line(revision: u64) -> Vec<u8> {
    let mut bytes = valid(revision);
    bytes.push(b'\n');
    bytes
}

/// Fixture sintetica dello schema 0: il solo `schemaVersion`, senza a capo.
const V0: &[u8] = br#"{"schemaVersion":0}"#;

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
            is_reserved_temporary(&name).then(|| name.to_string_lossy().into_owned())
        })
        .collect()
}

fn preserved_copies(directory: &Path) -> Vec<String> {
    entries(directory)
        .into_iter()
        .filter(|name| name.starts_with("config.corrupt."))
        .collect()
}

fn write_action(error: ConfigError) -> &'static str {
    match error {
        ConfigError::Write { action, .. } => action,
        other => panic!("errore inatteso: {other}"),
    }
}

fn open_with_startup(root: VerifiedRoot) -> (Store, super::Startup) {
    let locked = platform::lock(root).expect("lock di istanza acquisito");
    Store::open(locked, COHERENT).expect("apertura riuscita")
}

fn open_store(root: VerifiedRoot) -> (Store, StartupKind) {
    let (store, startup) = open_with_startup(root);
    (store, startup.kind)
}

#[test]
fn serialization_writes_the_exact_v1_header() {
    assert_eq!(
        document::serialize(7).expect("serializzato"),
        b"{\"schemaVersion\":1,\"revision\":7}\n".to_vec()
    );
    match document::parse(&document::serialize(0).expect("serializzato")) {
        Parsed::Current(config) => {
            assert_eq!(config.schema_version(), 1);
            assert_eq!(config.revision(), 0);
        }
        _ => panic!("la generazione serializzata non viene riletta"),
    }
}

#[test]
fn temporaries_are_recognised_only_by_the_exact_pattern() {
    for name in [".config.1.0.tmp", ".config.4242.17.tmp", ".config.0.0.tmp"] {
        assert!(is_reserved_temporary(OsStr::new(name)), "{name}");
    }
    for name in [
        "config.json",
        "config.json.prev",
        "config.corrupt.1.0",
        "notes.txt",
        ".config.1.tmp",
        ".config.tmp",
        ".config..1.tmp",
        ".config.1..tmp",
        ".config.1.0.tmp.bak",
        ".config.1.0.2.tmp",
        ".config.a.0.tmp",
        ".config.1.-1.tmp",
        ".config. 1.0.tmp",
        "x.config.1.0.tmp",
        ".config.1.0.TMP",
    ] {
        assert!(!is_reserved_temporary(OsStr::new(name)), "{name}");
    }
}

#[test]
fn temporary_collisions_are_retried_up_to_the_limit() {
    let scratch = Scratch::new("collision");
    let root = scratch.root();
    write(&root, ".config.7.0.tmp", b"occupato");
    let before = entries(root.path());

    let error = match create_temporary_with(&root, Step::CreateTemp, || {
        OsString::from(".config.7.0.tmp")
    }) {
        Ok(_) => panic!("collisione non rilevata"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ConfigError::TemporaryCollision { attempts: 16 }
    ));
    assert_eq!(entries(root.path()), before, "nessun nome nuovo creato");
}

#[test]
fn temporary_creation_skips_collisions_and_cleans_up_on_drop() {
    let scratch = Scratch::new("skip-collision");
    let root = scratch.root();
    write(&root, ".config.7.0.tmp", b"occupato");

    let mut names = [
        OsString::from(".config.7.0.tmp"),
        OsString::from(".config.7.1.tmp"),
    ]
    .into_iter();
    let (pending, mut file) = match create_temporary_with(&root, Step::CreateTemp, || {
        names.next().expect("nome disponibile")
    }) {
        Ok(created) => created,
        Err(error) => panic!("creazione fallita: {error}"),
    };
    file.write_all(b"contenuto").expect("contenuto scritto");
    drop(file);
    assert_eq!(read(root.path(), ".config.7.1.tmp"), b"contenuto");

    drop(pending);
    assert!(
        !root.path().join(".config.7.1.tmp").exists(),
        "il temporaneo viene rimosso"
    );
    assert_eq!(
        read(root.path(), ".config.7.0.tmp"),
        b"occupato",
        "la rimozione del PendingFile non tocca un nome già occupato"
    );
}

const FIRST_RUN_STEPS: [(Step, &str); 4] = [
    (Step::CreateTemp, "creazione del temporaneo"),
    (Step::WriteTemp, "scrittura del temporaneo"),
    (Step::SyncTemp, "sincronizzazione del temporaneo"),
    (Step::PublishMain, "pubblicazione del principale"),
];

#[test]
fn first_creation_failures_are_not_committed_and_leave_no_configuration() {
    for (step, action) in FIRST_RUN_STEPS {
        let scratch = Scratch::new(&format!("first-run-{step:?}"));
        let root = scratch.root();
        let directory = root.path().to_path_buf();

        fault::arm(step);
        let locked = platform::lock(root).expect("lock di istanza acquisito");
        let error = match Store::open(locked, COHERENT) {
            Ok(_) => panic!("{step:?}: il guasto non è stato rilevato"),
            Err(error) => error,
        };
        fault::clear();

        assert_eq!(write_action(error), action, "{step:?}");
        assert_eq!(
            entries(&directory),
            vec![platform::LOCK_FILE.to_owned()],
            "{step:?}: resta soltanto il file di lock, che non viene mai rimosso"
        );
    }
}

#[test]
fn first_creation_commit_without_directory_sync_is_reported_not_retried() {
    let scratch = Scratch::new("first-run-sync");
    let root = scratch.root();
    let directory = root.path().to_path_buf();

    fault::arm(Step::SyncDirectory);
    let (store, kind) = open_store(root);
    fault::clear();

    match kind {
        StartupKind::FirstRun { commit } => {
            assert_eq!(commit.revision(), 0);
            assert!(
                matches!(commit, Commit::CommittedNotDurable { .. }),
                "la durabilità non è confermata"
            );
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(store.config().revision(), 0);
    assert_eq!(read(&directory, CONFIG_FILE), line(0));
    assert!(!directory.join(CONFIG_BACKUP_FILE).exists());

    let commit = store.save().expect("salvataggio successivo");
    assert_eq!(commit.revision(), 1);
    assert_eq!(store.config().revision(), 1);
    assert_eq!(read(&directory, CONFIG_FILE), line(1));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), line(0));
    assert!(reserved_temporaries(&directory).is_empty());
}

const SAVE_STEPS: [(Step, &str); 8] = [
    (Step::CreateTemp, "creazione del temporaneo"),
    (Step::WriteTemp, "scrittura del temporaneo"),
    (Step::SyncTemp, "sincronizzazione del temporaneo"),
    (Step::CreateBackupTemp, "creazione del temporaneo"),
    (Step::WriteBackupTemp, "scrittura del temporaneo"),
    (Step::SyncBackupTemp, "sincronizzazione del temporaneo"),
    (Step::ReplaceBackup, "sostituzione del backup"),
    (Step::PublishMain, "sostituzione del principale"),
];

#[test]
fn save_failures_before_the_commit_leave_the_main_intact() {
    for (step, action) in SAVE_STEPS {
        let scratch = Scratch::new(&format!("save-{step:?}"));
        let root = scratch.root();
        let directory = root.path().to_path_buf();
        let (store, kind) = open_store(root);
        assert!(matches!(kind, StartupKind::FirstRun { .. }));

        fault::arm(step);
        let error = store
            .save()
            .expect_err("il guasto deve fallire il salvataggio");
        fault::clear();

        assert_eq!(write_action(error), action, "{step:?}");
        assert_eq!(
            store.config().revision(),
            0,
            "{step:?}: la revisione in memoria non avanza"
        );
        assert_eq!(
            read(&directory, CONFIG_FILE),
            line(0),
            "{step:?}: il principale resta la generazione precedente"
        );
        if step == Step::PublishMain {
            assert_eq!(
                read(&directory, CONFIG_BACKUP_FILE),
                line(0),
                "il backup preparato contiene la generazione corrente"
            );
        } else {
            assert!(
                !directory.join(CONFIG_BACKUP_FILE).exists(),
                "{step:?}: nessun backup pubblicato"
            );
        }
        assert!(
            reserved_temporaries(&directory).is_empty(),
            "{step:?}: nessun temporaneo proprio resta, trovato {:?}",
            reserved_temporaries(&directory)
        );

        let commit = store.save().expect("salvataggio successivo riuscito");
        assert_eq!(commit.revision(), 1);
        assert_eq!(store.config().revision(), 1);
        assert_eq!(read(&directory, CONFIG_FILE), line(1));
        assert_eq!(read(&directory, CONFIG_BACKUP_FILE), line(0));
    }
}

#[test]
fn save_commit_without_directory_sync_is_reported_and_not_repeated() {
    let scratch = Scratch::new("save-sync");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let (store, _) = open_store(root);

    fault::arm(Step::SyncDirectory);
    let commit = store.save().expect("il commit non è un errore");
    fault::clear();

    assert!(
        matches!(commit, Commit::CommittedNotDurable { revision: 1, .. }),
        "esito inatteso: {commit:?}"
    );
    assert_eq!(store.config().revision(), 1);
    assert_eq!(read(&directory, CONFIG_FILE), line(1));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), line(0));

    let next = store.save().expect("secondo salvataggio");
    assert_eq!(next.revision(), 2);
    assert_eq!(store.config().revision(), 2);
    assert_eq!(read(&directory, CONFIG_FILE), line(2));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), line(1));
}

const MIGRATION_STEPS: [(Step, &str); 8] = [
    (Step::CreateTemp, "creazione del temporaneo"),
    (Step::WriteTemp, "scrittura del temporaneo"),
    (Step::SyncTemp, "sincronizzazione del temporaneo"),
    (Step::CreateBackupTemp, "creazione del temporaneo"),
    (Step::WriteBackupTemp, "scrittura del temporaneo"),
    (Step::SyncBackupTemp, "sincronizzazione del temporaneo"),
    (Step::ReplaceBackup, "sostituzione del backup"),
    (Step::PublishMain, "sostituzione del principale"),
];

#[test]
fn migration_failures_before_the_commit_preserve_the_original_generation() {
    for (step, action) in MIGRATION_STEPS {
        // L'apertura fallita rilascia il lock e il tentativo successivo lo
        // riacquisisce: il mutex impedisce che un avvio di figlio di un'altra
        // prova cada fra i due.
        let _forks = fork_window();
        let scratch = Scratch::new(&format!("migrate-main-{step:?}"));
        let root = scratch.root();
        let directory = root.path().to_path_buf();
        write(&root, CONFIG_FILE, V0);

        fault::arm(step);
        let locked = platform::lock(root).expect("lock di istanza acquisito");
        let error = match Store::open(locked, COHERENT) {
            Ok(_) => panic!("{step:?}: il guasto non è stato rilevato"),
            Err(error) => error,
        };
        fault::clear();

        assert_eq!(write_action(error), action, "{step:?}");
        assert_eq!(
            read(&directory, CONFIG_FILE),
            V0,
            "{step:?}: la sorgente non viene sovrascritta prima del commit"
        );
        if step == Step::PublishMain {
            assert_eq!(
                read(&directory, CONFIG_BACKUP_FILE),
                V0,
                "il backup preparato conserva i byte originali"
            );
        } else {
            assert!(
                !directory.join(CONFIG_BACKUP_FILE).exists(),
                "{step:?}: nessun backup pubblicato"
            );
        }
        assert!(
            reserved_temporaries(&directory).is_empty(),
            "{step:?}: {temporaries:?}",
            temporaries = reserved_temporaries(&directory)
        );

        let (store, kind) = open_store(scratch.root());
        match kind {
            StartupKind::Migrated {
                source: MigrationSource::Main,
                from_schema_version: 0,
                previous_revision: 0,
                preserved: None,
                commit,
            } => assert_eq!(commit.revision(), 1),
            other => panic!("{step:?}: esito inatteso: {other:?}"),
        }
        assert_eq!(store.config().revision(), 1, "{step:?}");
        assert_eq!(read(&directory, CONFIG_FILE), line(1), "{step:?}");
        assert_eq!(read(&directory, CONFIG_BACKUP_FILE), V0, "{step:?}");
    }
}

const BACKUP_MIGRATION_STEPS: [(Step, &str); 9] = [
    (
        Step::PreserveCorrupt,
        "conservazione del principale corrotto",
    ),
    (Step::CreateTemp, "creazione del temporaneo"),
    (Step::WriteTemp, "scrittura del temporaneo"),
    (Step::SyncTemp, "sincronizzazione del temporaneo"),
    (Step::CreateBackupTemp, "creazione del temporaneo"),
    (Step::WriteBackupTemp, "scrittura del temporaneo"),
    (Step::SyncBackupTemp, "sincronizzazione del temporaneo"),
    (Step::ReplaceBackup, "sostituzione del backup"),
    (Step::PublishMain, "sostituzione del principale"),
];

#[test]
fn backup_migration_failures_never_lose_the_corrupt_main() {
    for (step, action) in BACKUP_MIGRATION_STEPS {
        let _forks = fork_window();
        let scratch = Scratch::new(&format!("migrate-backup-{step:?}"));
        let root = scratch.root();
        let directory = root.path().to_path_buf();
        let corrupt = b"{\"schemaVersion\":1,".to_vec();
        write(&root, CONFIG_FILE, &corrupt);
        write(&root, CONFIG_BACKUP_FILE, V0);

        fault::arm(step);
        let locked = platform::lock(root).expect("lock di istanza acquisito");
        let error = match Store::open(locked, COHERENT) {
            Ok(_) => panic!("{step:?}: il guasto non è stato rilevato"),
            Err(error) => error,
        };
        fault::clear();

        assert_eq!(write_action(error), action, "{step:?}");
        assert_eq!(read(&directory, CONFIG_FILE), corrupt, "{step:?}");
        assert_eq!(read(&directory, CONFIG_BACKUP_FILE), V0, "{step:?}");
        assert!(reserved_temporaries(&directory).is_empty(), "{step:?}");
        // La copia esiste solo se il guasto interviene dopo la sua scrittura e
        // sincronizzazione: `PreserveCorrupt` la precede e `WriteTemp`/`SyncTemp`
        // colpiscono la copia stessa, che viene rimossa se incompleta.
        let copy_complete = !matches!(
            step,
            Step::PreserveCorrupt | Step::WriteTemp | Step::SyncTemp
        );
        if copy_complete {
            let preserved = preserved_copies(&directory);
            assert_eq!(preserved.len(), 1, "{step:?}");
            assert_eq!(read(&directory, &preserved[0]), corrupt, "{step:?}");
        } else {
            assert!(preserved_copies(&directory).is_empty(), "{step:?}");
        }

        let (store, kind) = open_store(scratch.root());
        match kind {
            StartupKind::Migrated {
                source: MigrationSource::Backup,
                from_schema_version: 0,
                previous_revision: 0,
                preserved: Some(_),
                commit,
            } => assert_eq!(commit.revision(), 1),
            other => panic!("{step:?}: esito inatteso: {other:?}"),
        }
        assert_eq!(store.config().revision(), 1, "{step:?}");
        assert_eq!(read(&directory, CONFIG_FILE), line(1), "{step:?}");
        assert_eq!(read(&directory, CONFIG_BACKUP_FILE), V0, "{step:?}");
        // Il recupero del tentativo riuscito conserva di nuovo il principale
        // corrotto, che era ancora al suo posto.
        let expected_copies = if copy_complete { 2 } else { 1 };
        assert_eq!(
            preserved_copies(&directory).len(),
            expected_copies,
            "{step:?}"
        );
    }
}

#[test]
fn migration_commit_without_directory_sync_is_reported_not_retried() {
    let scratch = Scratch::new("migrate-sync");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, V0);

    fault::arm(Step::SyncDirectory);
    let (store, kind) = open_store(root);
    fault::clear();

    match kind {
        StartupKind::Migrated { commit, .. } => {
            assert_eq!(commit.revision(), 1);
            assert!(
                matches!(commit, Commit::CommittedNotDurable { revision: 1, .. }),
                "esito inatteso: {commit:?}"
            );
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(store.config().revision(), 1);
    assert_eq!(read(&directory, CONFIG_FILE), line(1));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), V0);

    let next = store.save().expect("salvataggio successivo");
    assert_eq!(next.revision(), 2);
    assert_eq!(read(&directory, CONFIG_FILE), line(2));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), line(1));
}

#[test]
fn recovery_preserves_the_corrupt_main_and_keeps_the_backup_revision() {
    let scratch = Scratch::new("recover-corrupt");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let corrupt = b"{\"schemaVersion\":1,".to_vec();
    write(&root, CONFIG_FILE, &corrupt);
    write(&root, CONFIG_BACKUP_FILE, &valid(4));

    let (store, kind) = open_store(root);
    let (preserved, revision) = match kind {
        StartupKind::Recovered {
            backup,
            main_failure,
            preserved,
            commit,
        } => {
            assert_eq!(backup.revision(), 4);
            assert!(matches!(main_failure, GenerationFailure::Corrupt { .. }));
            assert_eq!(commit.revision(), 4);
            (preserved.expect("copia conservata"), backup.revision())
        }
        other => panic!("esito inatteso: {other:?}"),
    };

    assert_eq!(store.config().revision(), revision);
    assert_eq!(read(&directory, CONFIG_FILE), valid(4));
    assert_eq!(
        read(&directory, CONFIG_BACKUP_FILE),
        valid(4),
        "il backup resta la generazione recuperata"
    );
    let preserved = preserved.to_string_lossy().into_owned();
    assert!(
        preserved.starts_with("config.corrupt."),
        "nome inatteso: {preserved}"
    );
    assert_eq!(
        read(&directory, &preserved),
        corrupt,
        "la copia conserva i byte del principale corrotto"
    );
    assert!(reserved_temporaries(&directory).is_empty());

    let (store, kind) = {
        let _forks = fork_window();
        drop(store);
        open_store(scratch.root())
    };
    assert_eq!(store.config().revision(), 4);
    assert!(matches!(kind, StartupKind::Normal { .. }));
}

#[test]
fn recovery_from_an_absent_main_has_no_preserved_copy() {
    let scratch = Scratch::new("recover-absent");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_BACKUP_FILE, &valid(4));

    let (store, kind) = open_store(root);
    match kind {
        StartupKind::Recovered {
            backup,
            main_failure,
            preserved,
            commit,
        } => {
            assert_eq!(backup.revision(), 4);
            assert_eq!(main_failure, GenerationFailure::Absent);
            assert!(preserved.is_none(), "nessun principale da conservare");
            assert_eq!(commit.revision(), 4);
        }
        other => panic!("esito inatteso: {other:?}"),
    }

    assert_eq!(store.config().revision(), 4);
    assert_eq!(read(&directory, CONFIG_FILE), valid(4));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), valid(4));
    assert!(preserved_copies(&directory).is_empty());
    assert_eq!(
        entries(&directory),
        vec![
            CONFIG_FILE.to_owned(),
            CONFIG_BACKUP_FILE.to_owned(),
            platform::LOCK_FILE.to_owned(),
        ]
    );
}

const RECOVERY_STEPS: [(Step, &str); 5] = [
    (
        Step::PreserveCorrupt,
        "conservazione del principale corrotto",
    ),
    (Step::CreateTemp, "creazione del temporaneo"),
    (Step::WriteTemp, "scrittura del temporaneo"),
    (Step::SyncTemp, "sincronizzazione del temporaneo"),
    (Step::PublishMain, "sostituzione del principale"),
];

#[test]
fn recovery_failures_never_lose_the_corrupt_main() {
    for (step, action) in RECOVERY_STEPS {
        // L'apertura fallita rilascia il lock e il tentativo successivo lo
        // riacquisisce: il mutex impedisce che un avvio di figlio di un'altra
        // prova cada fra i due.
        let _forks = fork_window();
        let scratch = Scratch::new(&format!("recover-{step:?}"));
        let root = scratch.root();
        let directory = root.path().to_path_buf();
        let corrupt = b"{\"schemaVersion\":1,".to_vec();
        write(&root, CONFIG_FILE, &corrupt);
        write(&root, CONFIG_BACKUP_FILE, &valid(4));

        fault::arm(step);
        let locked = platform::lock(root).expect("lock di istanza acquisito");
        let error = match Store::open(locked, COHERENT) {
            Ok(_) => panic!("{step:?}: il guasto non è stato rilevato"),
            Err(error) => error,
        };
        fault::clear();

        assert_eq!(write_action(error), action, "{step:?}");
        assert_eq!(read(&directory, CONFIG_FILE), corrupt);
        assert_eq!(read(&directory, CONFIG_BACKUP_FILE), valid(4));
        assert!(reserved_temporaries(&directory).is_empty(), "{step:?}");
        let preserved = preserved_copies(&directory);
        // La copia esiste solo se il guasto interviene dopo la sua scrittura:
        // `WriteTemp` e `SyncTemp` colpiscono la scrittura della copia stessa.
        if matches!(step, Step::CreateTemp | Step::PublishMain) {
            assert_eq!(preserved.len(), 1, "{step:?}");
            assert_eq!(read(&directory, &preserved[0]), corrupt, "{step:?}");
        } else {
            assert!(preserved.is_empty(), "{step:?}: nessuna copia completa");
        }

        // Un nuovo tentativo senza guasti recupera la stessa revisione.
        let (store, kind) = open_store(scratch.root());
        assert!(
            matches!(kind, StartupKind::Recovered { .. }),
            "{step:?}: {kind:?}"
        );
        assert_eq!(store.config().revision(), 4, "{step:?}");
        assert_eq!(read(&directory, CONFIG_FILE), valid(4), "{step:?}");
    }
}

#[test]
fn recovery_commit_without_directory_sync_is_reported_not_retried() {
    let scratch = Scratch::new("recover-sync");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, b"{\"schemaVersion\":1,");
    write(&root, CONFIG_BACKUP_FILE, &valid(4));

    fault::arm(Step::SyncDirectory);
    let (store, kind) = open_store(root);
    fault::clear();

    match kind {
        StartupKind::Recovered { commit, .. } => {
            assert_eq!(commit.revision(), 4);
            assert!(
                matches!(commit, Commit::CommittedNotDurable { revision: 4, .. }),
                "esito inatteso: {commit:?}"
            );
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(store.config().revision(), 4);
    assert_eq!(read(&directory, CONFIG_FILE), valid(4));

    let commit = store.save().expect("salvataggio successivo");
    assert_eq!(commit.revision(), 5);
    assert_eq!(read(&directory, CONFIG_FILE), line(5));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), valid(4));
}

#[test]
fn an_existing_main_is_never_overwritten_during_first_creation() {
    let scratch = Scratch::new("appeared");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, &valid(9));

    let error = match write::create_initial(&root, ConfigV1::initial()) {
        Ok(_) => panic!("la destinazione esistente è stata sovrascritta"),
        Err(error) => error,
    };
    match error {
        ConfigError::AppearedDuringFirstRun { path } => {
            assert_eq!(path, directory.join(CONFIG_FILE));
        }
        other => panic!("errore inatteso: {other}"),
    }
    assert_eq!(read(&directory, CONFIG_FILE), valid(9));
    assert!(!directory.join(CONFIG_BACKUP_FILE).exists());
    assert!(reserved_temporaries(&directory).is_empty());
}

#[test]
fn stale_temporaries_are_not_read_and_are_discarded_before_first_creation() {
    let scratch = Scratch::new("stale");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    // Il PID diverso da quello del processo di prova appartiene comunque al
    // namespace riservato: con un solo scrittore è un residuo da scartare.
    write(&root, ".config.4242.0.tmp", &valid(7));
    write(&root, "config.json.old", b"vecchio");
    write(&root, "notes.txt", b"nota");
    write(&root, "config.corrupt.1.0", b"diagnostica");
    // Nomi simili ma fuori dal namespace: non vengono mai rimossi.
    write(&root, "x.config.1.0.tmp", b"estraneo");
    write(&root, ".config.1.0.tmp.bak", b"copia");

    let (store, kind) = open_store(root);
    match kind {
        StartupKind::FirstRun { .. } => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(
        store.config().revision(),
        0,
        "il temporaneo non viene mai applicato"
    );
    assert_eq!(read(&directory, CONFIG_FILE), line(0));
    assert!(!directory.join(".config.4242.0.tmp").exists());
    assert_eq!(read(&directory, "config.json.old"), b"vecchio");
    assert_eq!(read(&directory, "notes.txt"), b"nota");
    assert_eq!(read(&directory, "config.corrupt.1.0"), b"diagnostica");
    assert_eq!(read(&directory, "x.config.1.0.tmp"), b"estraneo");
    assert_eq!(read(&directory, ".config.1.0.tmp.bak"), b"copia");
    assert!(reserved_temporaries(&directory).is_empty());
}

// ---------------------------------------------------------------------------
// Arresto del processo nei punti critici (processi figli)
// ---------------------------------------------------------------------------

const CRASH_ENV: &str = "TOTUM_TEST_CRASH_SCENARIO";
const CRASH_DIR_ENV: &str = "TOTUM_TEST_CRASH_DIR";
const CRASH_TEST: &str = "config::write_tests::crash_child";

/// Processo figlio che termina senza unwinding al punto armato.
///
/// Eseguito soltanto dal processo padre con `--ignored --exact`; senza la
/// variabile d'ambiente termina subito. L'arresto è `std::process::abort()`:
/// nessun `Drop` rimuove i temporanei, come in un processo terminato dal
/// sistema. Non è una prova di perdita di alimentazione.
#[test]
#[ignore = "eseguito soltanto dai processi figli delle prove di arresto"]
fn crash_child() {
    let Ok(scenario) = std::env::var(CRASH_ENV) else {
        return;
    };
    let base = PathBuf::from(std::env::var(CRASH_DIR_ENV).expect("directory del figlio"));
    let root = platform::prepare_root(&base.join("totum")).expect("radice del figlio preparabile");

    match scenario.as_str() {
        "first-run-write-temp" => {
            fault::arm_crash(Step::WriteTemp);
            let locked = platform::lock(root).expect("lock di istanza acquisito");
            let _ = Store::open(locked, COHERENT);
        }
        "save-backup-replaced" => {
            let (store, _) = open_store(root);
            fault::arm_crash(Step::PublishMain);
            let _ = store.save();
        }
        "save-published" => {
            let (store, _) = open_store(root);
            fault::arm_crash(Step::SyncDirectory);
            let _ = store.save();
        }
        "recover-unpublished" => {
            write(&root, CONFIG_FILE, b"{\"schemaVersion\":1,");
            write(&root, CONFIG_BACKUP_FILE, &valid(4));
            fault::arm_crash(Step::PublishMain);
            let locked = platform::lock(root).expect("lock di istanza acquisito");
            let _ = Store::open(locked, COHERENT);
        }
        "migration-unpublished" => {
            write(&root, CONFIG_FILE, V0);
            fault::arm_crash(Step::PublishMain);
            let locked = platform::lock(root).expect("lock di istanza acquisito");
            let _ = Store::open(locked, COHERENT);
        }
        "migration-published" => {
            write(&root, CONFIG_FILE, V0);
            fault::arm_crash(Step::SyncDirectory);
            let locked = platform::lock(root).expect("lock di istanza acquisito");
            let _ = Store::open(locked, COHERENT);
        }
        other => panic!("scenario di arresto sconosciuto: {other}"),
    }

    // Se il processo non è stato arrestato dal punto armato, il padre deve
    // vedere un esito fallito invece di un falso positivo.
    std::process::exit(17);
}

/// Avvia il figlio e attende la terminazione entro un termine massimo; in caso
/// di superamento lo uccide e fallisce la prova. La pulizia della directory
/// resta al `Drop` di [`Scratch`] del processo padre.
fn run_crash_child(scenario: &str, base: &Path) {
    let exe = std::env::current_exe().expect("binario di prova");
    let mut child = spawn(
        Command::new(exe)
            .args(["--ignored", "--exact", CRASH_TEST])
            .env(CRASH_ENV, scenario)
            .env(CRASH_DIR_ENV, base)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .expect("processo figlio avviato");

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait().expect("stato del processo figlio") {
            Some(status) => {
                assert!(
                    !status.success(),
                    "{scenario}: il figlio doveva terminare senza unwinding"
                );
                return;
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{scenario}: termine massimo superato");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

#[test]
fn a_crash_before_publication_leaves_only_a_discardable_temporary() {
    let scratch = Scratch::new("crash-first-run");
    run_crash_child("first-run-write-temp", &scratch.path);
    let directory = scratch.path.join("totum");

    assert!(!directory.join(CONFIG_FILE).exists(), "nulla è pubblicato");
    let temporaries = reserved_temporaries(&directory);
    assert_eq!(temporaries.len(), 1, "{temporaries:?}");
    assert_eq!(
        read(&directory, &temporaries[0]),
        b"",
        "il temporaneo è incompleto e non pubblicato"
    );

    let (store, startup) = open_with_startup(scratch.root());
    assert!(matches!(startup.kind, StartupKind::FirstRun { .. }));
    assert_eq!(startup.discarded_temporaries.len(), 1);
    assert_eq!(store.config().revision(), 0);
    assert_eq!(read(&directory, CONFIG_FILE), line(0));
    assert!(!directory.join(CONFIG_BACKUP_FILE).exists());
    assert!(reserved_temporaries(&directory).is_empty());
}

#[test]
fn a_crash_after_the_backup_replacement_leaves_the_previous_main() {
    let scratch = Scratch::new("crash-save-backup");
    run_crash_child("save-backup-replaced", &scratch.path);
    let directory = scratch.path.join("totum");

    assert_eq!(read(&directory, CONFIG_FILE), line(0));
    assert_eq!(
        read(&directory, CONFIG_BACKUP_FILE),
        line(0),
        "il backup è già stato sostituito"
    );
    let temporaries = reserved_temporaries(&directory);
    assert_eq!(temporaries.len(), 1, "{temporaries:?}");
    assert_eq!(
        read(&directory, &temporaries[0]),
        line(1),
        "la nuova generazione non è stata applicata"
    );

    let (store, startup) = open_with_startup(scratch.root());
    assert!(matches!(
        startup.kind,
        StartupKind::Normal {
            backup: super::BackupState::Available { revision: 0 }
        }
    ));
    assert_eq!(startup.discarded_temporaries.len(), 1);
    assert_eq!(store.config().revision(), 0);
    assert!(reserved_temporaries(&directory).is_empty());

    let commit = store.save().expect("salvataggio successivo");
    assert_eq!(commit.revision(), 1);
    assert_eq!(read(&directory, CONFIG_FILE), line(1));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), line(0));
}

#[test]
fn a_crash_after_the_publication_leaves_the_committed_generation() {
    let scratch = Scratch::new("crash-save-published");
    run_crash_child("save-published", &scratch.path);
    let directory = scratch.path.join("totum");

    assert_eq!(read(&directory, CONFIG_FILE), line(1));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), line(0));
    assert!(
        reserved_temporaries(&directory).is_empty(),
        "il temporaneo pubblicato è stato rinominato"
    );

    let (store, startup) = open_with_startup(scratch.root());
    assert!(matches!(
        startup.kind,
        StartupKind::Normal {
            backup: super::BackupState::Available { revision: 0 }
        }
    ));
    assert!(startup.discarded_temporaries.is_empty());
    assert_eq!(store.config().revision(), 1);

    let commit = store.save().expect("salvataggio successivo");
    assert_eq!(commit.revision(), 2);
    assert_eq!(read(&directory, CONFIG_FILE), line(2));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), line(1));
}

#[test]
fn a_crash_after_preserving_the_corrupt_main_keeps_it_recoverable() {
    let scratch = Scratch::new("crash-recover");
    run_crash_child("recover-unpublished", &scratch.path);
    let directory = scratch.path.join("totum");
    let corrupt = b"{\"schemaVersion\":1,".to_vec();

    assert_eq!(read(&directory, CONFIG_FILE), corrupt);
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), valid(4));
    let preserved = preserved_copies(&directory);
    assert_eq!(preserved.len(), 1, "{preserved:?}");
    assert_eq!(read(&directory, &preserved[0]), corrupt);
    let temporaries = reserved_temporaries(&directory);
    assert_eq!(temporaries.len(), 1, "{temporaries:?}");
    assert_eq!(
        read(&directory, &temporaries[0]),
        valid(4),
        "il recupero non è stato pubblicato"
    );

    let (store, startup) = open_with_startup(scratch.root());
    assert!(matches!(
        startup.kind,
        StartupKind::Recovered {
            commit,
            ..
        } if commit.revision() == 4
    ));
    assert_eq!(startup.discarded_temporaries.len(), 1);
    assert_eq!(store.config().revision(), 4);
    assert_eq!(read(&directory, CONFIG_FILE), valid(4));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), valid(4));
    let preserved = preserved_copies(&directory);
    assert_eq!(
        preserved.len(),
        2,
        "il principale corrotto è conservato di nuovo"
    );
    for name in preserved {
        assert_eq!(read(&directory, &name), corrupt, "{name}");
    }

    let commit = store.save().expect("salvataggio successivo");
    assert_eq!(commit.revision(), 5);
    assert_eq!(read(&directory, CONFIG_FILE), line(5));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), valid(4));
}

#[test]
fn a_crash_before_the_migration_is_published_keeps_the_original_main() {
    let scratch = Scratch::new("crash-migration");
    run_crash_child("migration-unpublished", &scratch.path);
    let directory = scratch.path.join("totum");

    assert_eq!(read(&directory, CONFIG_FILE), V0);
    assert_eq!(
        read(&directory, CONFIG_BACKUP_FILE),
        V0,
        "il backup è già stato sostituito con i byte originali"
    );
    let temporaries = reserved_temporaries(&directory);
    assert_eq!(temporaries.len(), 1, "{temporaries:?}");
    assert_eq!(
        read(&directory, &temporaries[0]),
        line(1),
        "la generazione migrata non è stata applicata"
    );

    let (store, startup) = open_with_startup(scratch.root());
    assert!(
        matches!(
            startup.kind,
            StartupKind::Migrated {
                source: MigrationSource::Main,
                ..
            }
        ),
        "esito inatteso: {:?}",
        startup.kind
    );
    assert_eq!(startup.discarded_temporaries.len(), 1);
    assert_eq!(store.config().revision(), 1);
    assert_eq!(read(&directory, CONFIG_FILE), line(1));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), V0);
    assert!(reserved_temporaries(&directory).is_empty());
}

#[test]
fn a_crash_after_the_migration_is_published_leaves_the_migrated_generation() {
    let scratch = Scratch::new("crash-migration-published");
    run_crash_child("migration-published", &scratch.path);
    let directory = scratch.path.join("totum");

    assert_eq!(read(&directory, CONFIG_FILE), line(1));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), V0);
    assert!(
        reserved_temporaries(&directory).is_empty(),
        "il temporaneo pubblicato è stato rinominato"
    );

    let (store, startup) = open_with_startup(scratch.root());
    assert!(
        matches!(
            startup.kind,
            StartupKind::Normal {
                backup: super::BackupState::OlderSchema { schema_version: 0 }
            }
        ),
        "esito inatteso: {:?}",
        startup.kind
    );
    assert!(startup.discarded_temporaries.is_empty());
    assert_eq!(store.config().revision(), 1);

    let commit = store.save().expect("salvataggio successivo");
    assert_eq!(commit.revision(), 2);
    assert_eq!(read(&directory, CONFIG_FILE), line(2));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), line(1));
}
