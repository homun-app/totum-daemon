use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use super::{JOURNAL_FILE, Step, fault, reserved_temporaries};
use crate::journal::{Journal, JournalError, StartupKind};
use crate::platform::{self, LOCK_FILE, SharedRoot, VerifiedRoot};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("target/journal-unit-tests");
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn root(&self) -> PathBuf {
        self.0.join("state")
    }

    fn prepare(&self) -> VerifiedRoot {
        platform::prepare_root(&self.root()).unwrap()
    }

    fn shared(&self) -> SharedRoot {
        platform::lock(self.prepare()).unwrap().share()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn names(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn temporaries(directory: &Path) -> Vec<String> {
    names(directory)
        .into_iter()
        .filter(|name| name.starts_with(".journal.") && name.ends_with(".tmp"))
        .collect()
}

/// Scrive un journal reale con lo schema indicato, come farebbe una versione
/// futura o un artefatto estraneo, senza passare da `Journal::open`.
fn seed_schema(shared: &SharedRoot, schema: u64) {
    let path = shared.path().join(JOURNAL_FILE);
    let file = shared
        .verified_root()
        .create_read_write_file(OsStr::new(JOURNAL_FILE))
        .unwrap();
    let db = super::database(shared, file, path).unwrap();
    let transaction = db.begin_write().unwrap();
    {
        let mut meta = transaction.open_table(super::META).unwrap();
        meta.insert("schema", schema).unwrap();
        meta.insert("last", 0u64).unwrap();
        transaction.open_table(super::EVENTS).unwrap();
    }
    transaction.commit().unwrap();
}

fn crash(dir: &Scratch, step: &str) -> ExitStatus {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "journal::tests::crash_child"])
        .env("TOTUM_TEST_JOURNAL_CRASH_DIR", dir.root())
        .env("TOTUM_TEST_JOURNAL_CRASH_STEP", step)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "il figlio doveva interrompersi: {status}"
    );
    status
}

#[test]
#[ignore = "eseguito soltanto dai test padre come processo da interrompere"]
fn crash_child() {
    let Ok(directory) = std::env::var("TOTUM_TEST_JOURNAL_CRASH_DIR") else {
        return;
    };
    let Ok(step) = std::env::var("TOTUM_TEST_JOURNAL_CRASH_STEP") else {
        return;
    };
    let step = match step.as_str() {
        "create-temp" => Step::CreateTemp,
        "commit-schema" => Step::CommitSchema,
        "publish-main" => Step::PublishMain,
        "sync-directory" => Step::SyncDirectory,
        other => {
            eprintln!("scenario sconosciuto: {other}");
            std::process::exit(9);
        }
    };
    let root = platform::prepare_root(Path::new(&directory)).unwrap();
    let shared = platform::lock(root).unwrap().share();
    fault::arm_crash(step);
    let _ = Journal::open(shared);
    std::process::exit(17);
}

#[test]
fn a_crash_before_the_temporary_leaves_nothing_and_the_next_open_is_a_first_run() {
    let dir = Scratch::new();
    dir.prepare();
    crash(&dir, "create-temp");
    assert!(temporaries(&dir.root()).is_empty());
    assert_eq!(names(&dir.root()), vec![LOCK_FILE.to_owned()]);

    let (_, startup) = Journal::open(dir.shared()).unwrap();
    assert!(matches!(startup.kind, StartupKind::FirstRun { .. }));
    assert!(startup.discarded_temporaries.is_empty());
}

#[test]
fn a_crash_before_the_schema_commit_leaves_an_empty_residue() {
    let dir = Scratch::new();
    dir.prepare();
    crash(&dir, "commit-schema");
    let residue = temporaries(&dir.root());
    assert_eq!(residue.len(), 1, "residuo: {residue:?}");
    assert!(!dir.root().join(JOURNAL_FILE).exists());
    assert!(
        std::fs::metadata(dir.root().join(&residue[0]))
            .unwrap()
            .len()
            > 0,
        "il motore ha già scritto l'intestazione nel temporaneo"
    );

    let (mut journal, startup) = Journal::open(dir.shared()).unwrap();
    assert!(matches!(
        startup.kind,
        StartupKind::InterruptedBootstrap { .. }
    ));
    assert_eq!(startup.discarded_temporaries.len(), 1);
    assert!(temporaries(&dir.root()).is_empty());
    assert!(dir.root().join(JOURNAL_FILE).is_file());
    assert_eq!(journal.append(&[b"after-recovery"]).unwrap(), 1..=1);
}

#[test]
fn a_crash_after_the_commit_before_the_publication_keeps_the_temporary_only() {
    let dir = Scratch::new();
    dir.prepare();
    crash(&dir, "publish-main");
    let residue = temporaries(&dir.root());
    assert_eq!(residue.len(), 1, "residuo: {residue:?}");
    assert!(!dir.root().join(JOURNAL_FILE).exists());
    assert!(
        std::fs::metadata(dir.root().join(&residue[0]))
            .unwrap()
            .len()
            > 0,
        "il temporaneo deve contenere il commit dello schema"
    );

    let (mut journal, startup) = Journal::open(dir.shared()).unwrap();
    assert!(matches!(
        startup.kind,
        StartupKind::InterruptedBootstrap { .. }
    ));
    assert_eq!(
        startup.discarded_temporaries,
        vec![std::ffi::OsString::from(&residue[0])]
    );
    assert!(temporaries(&dir.root()).is_empty());
    // Il commit del temporaneo non è stato adottato: il journal riparte vuoto.
    assert!(journal.read_after(0, 1).unwrap().is_empty());
    assert_eq!(journal.append(&[b"fresh"]).unwrap(), 1..=1);
}

#[test]
fn a_crash_after_the_publication_before_the_directory_sync_leaves_the_main_open() {
    let dir = Scratch::new();
    dir.prepare();
    crash(&dir, "sync-directory");
    assert!(temporaries(&dir.root()).is_empty());
    assert!(dir.root().join(JOURNAL_FILE).is_file());

    let (mut journal, startup) = Journal::open(dir.shared()).unwrap();
    assert!(matches!(startup.kind, StartupKind::Existing { .. }));
    assert!(startup.discarded_temporaries.is_empty());
    assert!(journal.read_after(0, 1).unwrap().is_empty());
    assert_eq!(journal.append(&[b"one"]).unwrap(), 1..=1);
}

#[test]
fn a_future_schema_is_refused_without_touching_the_file() {
    let dir = Scratch::new();
    let shared = dir.shared();
    seed_schema(&shared, 2);
    let path = dir.root().join(JOURNAL_FILE);
    let before = std::fs::read(&path).unwrap();
    assert!(!before.is_empty());

    // La classificazione apre il motore in sola lettura: schema futuro e
    // corruzione non producono scritture, quindi i byte restano identici e il
    // rifiuto si ripete senza creare una nuova generazione.
    for _ in 0..2 {
        match Journal::open(shared.clone()) {
            Err(JournalError::FutureSchema { found: 2 }) => {}
            Err(other) => panic!("errore inatteso: {other:?}"),
            Ok(_) => panic!("uno schema futuro non deve essere aperto"),
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "il rifiuto non deve mutare il file"
        );
    }
    assert!(temporaries(&dir.root()).is_empty());
    assert!(
        reserved_temporaries(shared.verified_root())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_journal_left_unclean_is_repaired_and_the_mutation_is_reported() {
    let dir = Scratch::new();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "journal::tests::unclean_child"])
        .env("TOTUM_TEST_JOURNAL_CRASH_DIR", dir.root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "il figlio doveva interrompersi senza chiudere: {status}"
    );

    // Il commit del figlio è sincronizzato, ma il motore non ha registrato lo
    // stato dell'allocatore: la classificazione in sola lettura non può
    // leggere il file e l'apertura in scrittura esegue il ripristino, che
    // viene segnalato separatamente. I dati Totum non vengono reinterpretati.
    let (mut journal, startup) = Journal::open(dir.shared()).unwrap();
    match startup.kind {
        StartupKind::Existing { repaired: true } => {}
        other => panic!("ripristino non segnalato: {other:?}"),
    }
    assert_eq!(
        journal.read_after(0, 16).unwrap(),
        vec![super::Event {
            sequence: 1,
            payload: b"kept".to_vec(),
        }]
    );
    assert_eq!(journal.append(&[b"after-repair"]).unwrap(), 2..=2);
}

#[test]
#[ignore = "eseguito soltanto dai test padre come processo da interrompere"]
fn unclean_child() {
    let Ok(directory) = std::env::var("TOTUM_TEST_JOURNAL_CRASH_DIR") else {
        return;
    };
    let root = platform::prepare_root(Path::new(&directory)).unwrap();
    let shared = platform::lock(root).unwrap().share();
    let (mut journal, _) = Journal::open(shared).unwrap();
    journal.append(&[b"kept"]).unwrap();
    std::process::abort();
}

#[test]
fn a_corrupt_main_is_refused_without_being_truncated() {
    let dir = Scratch::new();
    let shared = dir.shared();
    let path = dir.root().join(JOURNAL_FILE);
    let file = shared
        .verified_root()
        .create_read_write_file(OsStr::new(JOURNAL_FILE))
        .unwrap();
    (&file)
        .write_all(b"not a redb database at all, just bytes")
        .unwrap();
    file.sync_all().unwrap();
    // Windows apre il file di creazione senza condivisione.
    drop(file);
    let mut before = Vec::new();
    std::fs::File::open(&path)
        .unwrap()
        .read_to_end(&mut before)
        .unwrap();

    match Journal::open(shared) {
        Err(JournalError::Corrupt { .. }) => {}
        Err(other) => panic!("errore inatteso: {other:?}"),
        Ok(_) => panic!("un journal corrotto non deve essere aperto"),
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(temporaries(&dir.root()).is_empty());
}

#[test]
fn an_empty_main_does_not_authorize_a_new_initialization() {
    let dir = Scratch::new();
    let shared = dir.shared();
    let file = shared
        .verified_root()
        .create_read_write_file(OsStr::new(JOURNAL_FILE))
        .unwrap();
    drop(file);

    match Journal::open(shared) {
        Err(JournalError::Corrupt { detail }) => {
            assert!(detail.contains("vuoto"), "diagnosi: {detail}");
        }
        Err(other) => panic!("errore inatteso: {other:?}"),
        Ok(_) => panic!("un journal vuoto non deve essere reinizializzato"),
    }
    assert_eq!(
        std::fs::metadata(dir.root().join(JOURNAL_FILE))
            .unwrap()
            .len(),
        0
    );
    assert!(temporaries(&dir.root()).is_empty());
}
