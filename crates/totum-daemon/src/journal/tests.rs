use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use redb::{ReadableDatabase as _, ReadableTableMetadata as _};

use super::{JOURNAL_FILE, Step, fault, reserved_temporaries};
use crate::journal::{
    Event, Journal, JournalBudget, JournalError, Position, ReducerState, RetentionLimits,
    RetentionOutcome, StartupKind,
};
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

fn event(sequence: u64, payload: &[u8]) -> Event {
    Event {
        sequence,
        payload: payload.to_vec(),
    }
}

/// Scrive un journal reale con il solo schema indicato, come farebbe una
/// versione futura o un artefatto estraneo, senza passare da `Journal::open`.
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
        meta.insert(super::META_SCHEMA, schema).unwrap();
        meta.insert(super::META_LAST, 0u64).unwrap();
        transaction.open_table(super::EVENTS).unwrap();
    }
    transaction.commit().unwrap();
}

/// Scrive un journal dello schema 1 della baseline: evento e posizione, senza
/// epoch, floor, tempi né snapshot.
fn seed_legacy(shared: &SharedRoot, events: &[(u64, &[u8])], last: u64) {
    let path = shared.path().join(JOURNAL_FILE);
    let file = shared
        .verified_root()
        .create_read_write_file(OsStr::new(JOURNAL_FILE))
        .unwrap();
    let db = super::database(shared, file, path).unwrap();
    let transaction = db.begin_write().unwrap();
    {
        let mut meta = transaction.open_table(super::META).unwrap();
        meta.insert(super::META_SCHEMA, super::LEGACY_SCHEMA_VERSION)
            .unwrap();
        meta.insert(super::META_LAST, last).unwrap();
        let mut table = transaction.open_table(super::EVENTS).unwrap();
        for (sequence, payload) in events {
            table.insert(*sequence, *payload).unwrap();
        }
    }
    transaction.commit().unwrap();
    drop(db);
}

fn force_meta(journal: &Journal, key: &str, value: u64) {
    let transaction = journal.db.begin_write().unwrap();
    {
        let mut meta = transaction.open_table(super::META).unwrap();
        meta.insert(key, value).unwrap();
    }
    transaction.commit().unwrap();
}

fn snapshot_count(journal: &Journal) -> u64 {
    let transaction = journal.db.begin_read().unwrap();
    transaction
        .open_table(super::SNAPSHOTS)
        .unwrap()
        .len()
        .unwrap()
}

fn replay(events: &[Event]) -> ReducerState {
    let mut state = ReducerState::default();
    for event in events {
        state.apply(&event.payload);
    }
    state
}

fn at(millis: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis)
}

fn unlimited_bytes() -> u64 {
    u64::MAX
}

fn run_child(test: &str, dir: &Scratch, step: &str) -> ExitStatus {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", test])
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

fn crash(dir: &Scratch, step: &str) -> ExitStatus {
    run_child("journal::tests::crash_child", dir, step)
}

fn lifecycle_crash(dir: &Scratch, step: &str) -> ExitStatus {
    run_child("journal::tests::lifecycle_crash_child", dir, step)
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
#[ignore = "eseguito soltanto dai test padre come processo da interrompere"]
fn lifecycle_crash_child() {
    let Ok(directory) = std::env::var("TOTUM_TEST_JOURNAL_CRASH_DIR") else {
        return;
    };
    let Ok(scenario) = std::env::var("TOTUM_TEST_JOURNAL_CRASH_STEP") else {
        return;
    };
    let (step, action) = match scenario.as_str() {
        "snapshot-before-commit" => (Step::SnapshotBeforeCommit, "snapshot"),
        "snapshot-after-commit" => (Step::SnapshotAfterCommit, "snapshot"),
        "retain-before-commit" => (Step::RetainBeforeCommit, "retain"),
        "retain-after-commit" => (Step::RetainAfterCommit, "retain"),
        "reset-before-commit" => (Step::ResetBeforeCommit, "reset"),
        "reset-after-commit" => (Step::ResetAfterCommit, "reset"),
        other => {
            eprintln!("scenario sconosciuto: {other}");
            std::process::exit(9);
        }
    };
    let root = platform::prepare_root(Path::new(&directory)).unwrap();
    let shared = platform::lock(root).unwrap().share();
    let (mut journal, _) = Journal::open(shared).unwrap();
    fault::arm_crash(step);
    match action {
        "snapshot" => {
            journal.snapshot().unwrap();
        }
        "retain" => {
            journal
                .retain(
                    RetentionLimits {
                        max_logical_bytes: 0,
                        max_age: Duration::MAX,
                    },
                    SystemTime::now(),
                )
                .unwrap();
        }
        _ => {
            journal.reset().unwrap();
        }
    }
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
    seed_schema(&shared, 3);
    let path = dir.root().join(JOURNAL_FILE);
    let before = std::fs::read(&path).unwrap();
    assert!(!before.is_empty());

    // La classificazione apre il motore in sola lettura: schema futuro e
    // corruzione non producono scritture, quindi i byte restano identici e il
    // rifiuto si ripete senza creare una nuova generazione.
    for _ in 0..2 {
        match Journal::open(shared.clone()) {
            Err(JournalError::FutureSchema { found: 3 }) => {}
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
fn a_current_schema_without_positions_is_corrupt() {
    let dir = Scratch::new();
    let shared = dir.shared();
    seed_schema(&shared, super::SCHEMA_VERSION);
    let before = std::fs::read(dir.root().join(JOURNAL_FILE)).unwrap();

    match Journal::open(shared) {
        Err(JournalError::Corrupt { detail }) => {
            assert!(detail.contains("epoch"), "diagnosi: {detail}");
        }
        Err(other) => panic!("errore inatteso: {other:?}"),
        Ok(_) => panic!("uno schema 2 senza posizioni non è valido"),
    }
    assert_eq!(
        std::fs::read(dir.root().join(JOURNAL_FILE)).unwrap(),
        before
    );
}

#[test]
fn a_legacy_schema_is_migrated_preserving_every_event_and_sequence() {
    let dir = Scratch::new();
    let shared = dir.shared();
    seed_legacy(&shared, &[(1, b"one"), (2, b"two"), (3, b"three")], 3);

    let (mut journal, startup) = Journal::open(shared).unwrap();
    assert_eq!(startup.migrated_from, Some(super::LEGACY_SCHEMA_VERSION));
    assert!(matches!(
        startup.kind,
        StartupKind::Existing { repaired: false }
    ));
    let epoch = journal.epoch().unwrap();
    assert_ne!(epoch, 0);
    let page = journal.read_from(Position::new(epoch, 0), 16).unwrap();
    assert_eq!(page.floor, 0);
    assert_eq!(page.high_water, 3);
    assert_eq!(
        page.events,
        vec![event(1, b"one"), event(2, b"two"), event(3, b"three")]
    );
    assert_eq!(journal.append(&[b"four"]).unwrap(), 4..=4);
    drop(journal);

    let (journal, startup) = Journal::open(dir.shared()).unwrap();
    assert_eq!(startup.migrated_from, None);
    assert_eq!(journal.epoch().unwrap(), epoch);
    assert_eq!(
        journal.read_after(0, 16).unwrap(),
        vec![
            event(1, b"one"),
            event(2, b"two"),
            event(3, b"three"),
            event(4, b"four"),
        ]
    );
    assert_eq!(journal.statistics().unwrap().high_water, 4);
}

#[test]
fn an_impossible_legacy_migration_is_refused_without_writing() {
    for events in [
        vec![(1u64, &b"one"[..]), (3, b"three")],
        vec![(1, b"one"), (2, b"two")],
    ] {
        let dir = Scratch::new();
        let shared = dir.shared();
        seed_legacy(&shared, &events, 3);
        let path = dir.root().join(JOURNAL_FILE);
        let before = std::fs::read(&path).unwrap();

        match Journal::open(shared) {
            Err(JournalError::Corrupt { detail }) => {
                assert!(
                    detail.contains("migrazione impossibile"),
                    "diagnosi: {detail}"
                );
            }
            Err(other) => panic!("errore inatteso: {other:?}"),
            Ok(_) => panic!("una migrazione impossibile non deve essere applicata"),
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(temporaries(&dir.root()).is_empty());
    }
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
    assert_eq!(journal.read_after(0, 16).unwrap(), vec![event(1, b"kept")]);
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
fn recovery_preserves_the_epoch() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"before"]).unwrap();
    drop(journal);

    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "journal::tests::unclean_child"])
        .env("TOTUM_TEST_JOURNAL_CRASH_DIR", dir.root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());

    let (journal, startup) = Journal::open(dir.shared()).unwrap();
    assert!(matches!(
        startup.kind,
        StartupKind::Existing { repaired: true }
    ));
    assert_eq!(journal.epoch().unwrap(), epoch);
    assert_eq!(
        journal.read_after(0, 16).unwrap(),
        vec![event(1, b"before"), event(2, b"kept")]
    );
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

#[test]
fn positions_outside_the_current_view_are_refused() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"one", b"two"]).unwrap();

    assert!(matches!(
        journal.read_from(Position::new(epoch, 3), 1),
        Err(JournalError::PositionBeyondEnd { high_water: 2, .. })
    ));
    assert!(matches!(
        journal.read_from(Position::new(epoch.wrapping_add(1), 0), 1),
        Err(JournalError::EpochChanged { expected, found })
            if expected == epoch && found == epoch.wrapping_add(1)
    ));
    assert!(matches!(
        journal.read_from(Position::new(epoch, 1), 0),
        Err(JournalError::InvalidPage)
    ));

    journal.snapshot().unwrap();
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 3,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RetentionOutcome::Advanced {
            from_floor: 0,
            to_floor: 1,
            ..
        }
    ));
    assert!(matches!(
        journal.read_from(Position::new(epoch, 0), 1),
        Err(JournalError::RetentionExpired { floor: 1, .. })
    ));
    let page = journal.read_from(Position::new(epoch, 1), 1).unwrap();
    assert_eq!(page.floor, 1);
    assert_eq!(page.events, vec![event(2, b"two")]);
}

#[test]
fn sequence_overflow_is_refused_without_advancing() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"kept"]).unwrap();
    force_meta(&journal, super::META_LAST, u64::MAX);

    assert!(matches!(
        journal.append(&[b"next"]),
        Err(JournalError::SequenceExhausted)
    ));
    assert_eq!(
        journal
            .read_from(Position::new(epoch, 0), 16)
            .unwrap()
            .events,
        vec![event(1, b"kept")]
    );
}

#[test]
fn epoch_overflow_refuses_reset_without_clearing_events() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    journal.append(&[b"kept"]).unwrap();
    force_meta(&journal, super::META_EPOCH, u64::MAX);

    assert!(matches!(journal.reset(), Err(JournalError::EpochExhausted)));
    assert_eq!(journal.epoch().unwrap(), u64::MAX);
    assert_eq!(
        journal
            .read_from(Position::new(u64::MAX, 0), 16)
            .unwrap()
            .events,
        vec![event(1, b"kept")]
    );
}

#[test]
fn the_epoch_is_stable_and_only_reset_changes_it() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"a", b"b"]).unwrap();
    journal.snapshot().unwrap();
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 0,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RetentionOutcome::Advanced { to_floor: 2, .. }
    ));
    assert_eq!(journal.epoch().unwrap(), epoch);
    drop(journal);

    let (mut journal, startup) = Journal::open(dir.shared()).unwrap();
    assert!(matches!(startup.kind, StartupKind::Existing { .. }));
    assert_eq!(journal.epoch().unwrap(), epoch);
    let stats = journal.statistics().unwrap();
    assert_eq!((stats.floor, stats.high_water, stats.events), (2, 2, 0));

    let next = journal.reset().unwrap();
    assert_ne!(next, epoch);
    assert_eq!(journal.epoch().unwrap(), next);
    assert_eq!(journal.latest_snapshot().unwrap(), None);
    let stats = journal.statistics().unwrap();
    assert_eq!((stats.floor, stats.high_water, stats.events), (0, 0, 0));
    assert!(matches!(
        journal.read_from(Position::new(epoch, 0), 1),
        Err(JournalError::EpochChanged { expected, found })
            if expected == next && found == epoch
    ));
    assert_eq!(journal.append(&[b"fresh"]).unwrap(), 1..=1);
    let page = journal.read_from(Position::new(next, 0), 16).unwrap();
    assert_eq!(page.epoch, next);
    assert_eq!(page.events, vec![event(1, b"fresh")]);
    drop(journal);

    let (journal, _) = Journal::open(dir.shared()).unwrap();
    assert_eq!(journal.epoch().unwrap(), next);
    assert_eq!(
        journal
            .read_from(Position::new(next, 0), 16)
            .unwrap()
            .events,
        vec![event(1, b"fresh")]
    );
}

#[test]
fn the_reducer_is_deterministic_and_order_sensitive() {
    let mut first = ReducerState::default();
    first.apply(b"alpha");
    first.apply(b"beta");
    let mut same = ReducerState::default();
    same.apply(b"alpha");
    same.apply(b"beta");
    assert_eq!(first, same);
    assert_eq!(first.count, 2);

    let mut reversed = ReducerState::default();
    reversed.apply(b"beta");
    reversed.apply(b"alpha");
    assert_ne!(first, reversed);
}

#[test]
fn snapshot_plus_suffix_equals_full_replay() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    journal.append(&[b"a", b"bb"]).unwrap();
    let early = journal.snapshot().unwrap();
    assert_eq!(early.applied_through, 2);
    journal.append(&[b"ccc"]).unwrap();
    let later = journal.snapshot().unwrap();
    assert_eq!(later.applied_through, 3);
    assert_eq!(later.epoch, early.epoch);

    let all = journal.read_after(0, 16).unwrap();
    assert_eq!(replay(&all), later.state);
    let mut resumed = early.state;
    for event in &all[2..] {
        resumed.apply(&event.payload);
    }
    assert_eq!(resumed, later.state);
    assert_eq!(journal.latest_snapshot().unwrap(), Some(later));

    let page = journal
        .read_from(Position::new(later.epoch, later.applied_through), 16)
        .unwrap();
    assert!(page.events.is_empty());
    assert_eq!(page.high_water, later.applied_through);
}

#[test]
fn at_most_two_snapshots_are_kept() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    for payload in [&b"one"[..], b"two", b"three"] {
        journal.append(&[payload]).unwrap();
        journal.snapshot().unwrap();
    }
    assert_eq!(snapshot_count(&journal), 2);
    let latest = journal.latest_snapshot().unwrap().unwrap();
    assert_eq!(latest.applied_through, 3);
    assert_eq!(latest.state, replay(&journal.read_after(0, 16).unwrap()));
}

#[test]
fn a_snapshot_crash_before_the_commit_leaves_no_snapshot() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"a", b"b", b"c"]).unwrap();
    drop(journal);

    lifecycle_crash(&dir, "snapshot-before-commit");

    let (mut journal, startup) = Journal::open(dir.shared()).unwrap();
    assert!(matches!(
        startup.kind,
        StartupKind::Existing { repaired: true }
    ));
    assert_eq!(journal.epoch().unwrap(), epoch);
    assert_eq!(journal.latest_snapshot().unwrap(), None);
    let events = journal.read_after(0, 16).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(journal.append(&[b"d"]).unwrap(), 4..=4);
    let snapshot = journal.snapshot().unwrap();
    assert_eq!(snapshot.applied_through, 4);
    assert_eq!(snapshot.state, replay(&journal.read_after(0, 16).unwrap()));
}

#[test]
fn a_snapshot_crash_after_the_commit_reopens_with_the_snapshot() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"a", b"b", b"c"]).unwrap();
    drop(journal);

    lifecycle_crash(&dir, "snapshot-after-commit");

    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    assert_eq!(journal.epoch().unwrap(), epoch);
    let snapshot = journal.latest_snapshot().unwrap().unwrap();
    assert_eq!(snapshot.epoch, epoch);
    assert_eq!(snapshot.applied_through, 3);
    assert_eq!(snapshot.state, replay(&journal.read_after(0, 16).unwrap()));
    assert_eq!(journal.append(&[b"d"]).unwrap(), 4..=4);
    let next = journal.snapshot().unwrap();
    assert_eq!(next.applied_through, 4);
    assert_eq!(next.state, replay(&journal.read_after(0, 16).unwrap()));
}

#[test]
fn a_retention_crash_before_the_commit_keeps_the_prefix() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"a", b"b", b"c"]).unwrap();
    journal.snapshot().unwrap();
    drop(journal);

    lifecycle_crash(&dir, "retain-before-commit");

    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    assert_eq!(journal.epoch().unwrap(), epoch);
    let stats = journal.statistics().unwrap();
    assert_eq!((stats.floor, stats.high_water, stats.events), (0, 3, 3));
    assert_eq!(journal.read_after(0, 16).unwrap().len(), 3);
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 0,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RetentionOutcome::Advanced { to_floor: 3, .. }
    ));
}

#[test]
fn a_retention_crash_after_the_commit_reopens_at_the_new_floor() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"a", b"b", b"c"]).unwrap();
    journal.snapshot().unwrap();
    drop(journal);

    lifecycle_crash(&dir, "retain-after-commit");

    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    assert_eq!(journal.epoch().unwrap(), epoch);
    let stats = journal.statistics().unwrap();
    assert_eq!((stats.floor, stats.high_water, stats.events), (3, 3, 0));
    assert!(
        journal
            .read_from(Position::new(epoch, 3), 16)
            .unwrap()
            .events
            .is_empty()
    );
    assert!(matches!(
        journal.read_from(Position::new(epoch, 0), 1),
        Err(JournalError::RetentionExpired { floor: 3, .. })
    ));
    assert_eq!(
        journal
            .latest_snapshot()
            .unwrap()
            .map(|snapshot| snapshot.applied_through),
        Some(3)
    );
    assert_eq!(journal.append(&[b"d"]).unwrap(), 4..=4);
    drop(journal);

    let (journal, _) = Journal::open(dir.shared()).unwrap();
    assert_eq!(journal.epoch().unwrap(), epoch);
    assert_eq!(journal.read_after(3, 16).unwrap(), vec![event(4, b"d")]);
}

#[test]
fn a_reset_crash_before_the_commit_keeps_the_old_generation() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"a", b"b"]).unwrap();
    drop(journal);

    lifecycle_crash(&dir, "reset-before-commit");

    let (journal, _) = Journal::open(dir.shared()).unwrap();
    assert_eq!(journal.epoch().unwrap(), epoch);
    assert_eq!(
        journal.read_after(0, 16).unwrap(),
        vec![event(1, b"a"), event(2, b"b")]
    );
}

#[test]
fn a_reset_crash_after_the_commit_reopens_in_the_new_generation() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"a", b"b"]).unwrap();
    drop(journal);

    lifecycle_crash(&dir, "reset-after-commit");

    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let next = journal.epoch().unwrap();
    assert_ne!(next, epoch);
    let stats = journal.statistics().unwrap();
    assert_eq!((stats.floor, stats.high_water, stats.events), (0, 0, 0));
    assert!(matches!(
        journal.read_from(Position::new(epoch, 0), 1),
        Err(JournalError::EpochChanged { .. })
    ));
    assert_eq!(journal.append(&[b"fresh"]).unwrap(), 1..=1);
}

/// Journal con un solo batch di payload da 4 byte l'uno, uno snapshot alla
/// vista corrente. Serve alle soglie per byte.
fn seeded(dir: &Scratch, payloads: &[&[u8]]) -> Journal {
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    journal.append(payloads).unwrap();
    journal.snapshot().unwrap();
    journal
}

fn retain_bytes(payloads: &[&[u8]], max_logical_bytes: u64) -> (RetentionOutcome, u64, u64) {
    let dir = Scratch::new();
    let mut journal = seeded(&dir, payloads);
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    let stats = journal.statistics().unwrap();
    (outcome, stats.floor, stats.logical_bytes)
}

#[test]
fn retention_byte_thresholds_are_exact() {
    let payloads: [&[u8]; 3] = [b"aaaa", b"bbbb", b"cccc"];
    // Dodici byte logici: la soglia è inclusa, un byte in meno avanza il floor
    // del minimo prefisso che la soddisfa.
    for (limit, floor, logical) in [
        (12, 0, 12),
        (11, 1, 8),
        (8, 1, 8),
        (7, 2, 4),
        (4, 2, 4),
        (0, 3, 0),
    ] {
        let (outcome, actual_floor, actual_logical) = retain_bytes(&payloads, limit);
        assert_eq!(
            (actual_floor, actual_logical),
            (floor, logical),
            "limite {limit}"
        );
        match outcome {
            RetentionOutcome::Unchanged { .. } => assert_eq!(floor, 0, "limite {limit}"),
            RetentionOutcome::Advanced {
                from_floor,
                to_floor,
                removed,
                ..
            } => {
                assert_eq!(from_floor, 0);
                assert_eq!(to_floor, floor);
                assert_eq!(removed, floor);
            }
            RetentionOutcome::Blocked { .. } => panic!("snapshot valido, nessun blocco"),
        }
    }
}

/// Journal con un evento da 4 byte per ogni tempo indicato, times forzati e uno
/// snapshot alla vista corrente. Il tempo di ingestione è quello esplicito.
fn aged(dir: &Scratch, times: &[u64]) -> Journal {
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    force_meta(&journal, super::META_INGESTED, 0);
    for time in times {
        journal.append_ingested(&[b"abcd"], *time).unwrap();
    }
    journal.snapshot().unwrap();
    journal
}

fn retain_age(times: &[u64], now_ms: u64, max_age_ms: u64) -> u64 {
    let dir = Scratch::new();
    let mut journal = aged(&dir, times);
    journal
        .retain(
            RetentionLimits {
                max_logical_bytes: unlimited_bytes(),
                max_age: Duration::from_millis(max_age_ms),
            },
            at(now_ms),
        )
        .unwrap();
    journal.statistics().unwrap().floor
}

#[test]
fn retention_age_thresholds_use_the_injected_clock() {
    let times = [1_000, 2_000, 3_000];
    // Orologio avanti: soglia inclusa e un millisecondo oltre.
    assert_eq!(retain_age(&times, 5_000, 3_001), 1);
    assert_eq!(retain_age(&times, 5_000, 3_000), 2);
    assert_eq!(retain_age(&times, 5_000, 2_001), 2);
    assert_eq!(retain_age(&times, 5_000, 2_000), 3);
    assert_eq!(retain_age(&times, 5_000, 1_999), 3);
    // Orologio indietro: un tempo di ingestione futuro non è vecchio e ferma
    // la soglia; nessun salto oltre il primo evento non ancora maturo.
    assert_eq!(retain_age(&times, 999, 0), 0);
    assert_eq!(retain_age(&times, 1_000, 0), 1);
}

#[test]
fn retention_is_blocked_until_a_snapshot_covers_the_prefix() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    journal.append(&[b"a", b"b", b"c"]).unwrap();
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 0,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RetentionOutcome::Blocked {
            required_floor: 3,
            snapshot_through: None,
            ..
        }
    ));
    let stats = journal.statistics().unwrap();
    assert_eq!((stats.floor, stats.events), (0, 3));

    drop(journal);
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    journal.append(&[b"a", b"b"]).unwrap();
    journal.snapshot().unwrap();
    journal.append(&[b"c"]).unwrap();
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 0,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RetentionOutcome::Blocked {
            required_floor: 3,
            snapshot_through: Some(2),
            ..
        }
    ));
    assert_eq!(journal.statistics().unwrap().events, 3);

    journal.snapshot().unwrap();
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 0,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RetentionOutcome::Advanced { to_floor: 3, .. }
    ));
}

#[test]
fn retention_removes_only_the_covered_prefix_and_continues_sequences() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"aaaa", b"bbbb", b"cccc"]).unwrap();
    journal.snapshot().unwrap();
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 8,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(matches!(
        outcome,
        RetentionOutcome::Advanced {
            from_floor: 0,
            to_floor: 1,
            removed: 1,
            logical_bytes_before: 12,
            logical_bytes_after: 8,
            ..
        }
    ));
    let stats = journal.statistics().unwrap();
    assert_eq!((stats.epoch, stats.floor, stats.high_water), (epoch, 1, 3));
    assert_eq!((stats.events, stats.logical_bytes), (2, 8));

    let page = journal.read_from(Position::new(epoch, 1), 16).unwrap();
    assert_eq!(page.events, vec![event(2, b"bbbb"), event(3, b"cccc")]);
    assert_eq!(journal.append(&[b"dddd"]).unwrap(), 4..=4);
    assert_eq!(journal.end_position().unwrap(), Position::new(epoch, 4));
    drop(journal);

    let (journal, _) = Journal::open(dir.shared()).unwrap();
    assert_eq!(journal.epoch().unwrap(), epoch);
    assert_eq!(
        journal.read_after(1, 16).unwrap(),
        vec![event(2, b"bbbb"), event(3, b"cccc"), event(4, b"dddd")]
    );
}

#[test]
fn logical_bytes_are_reported_separately_from_the_file_size() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    journal.append(&[b"0123456789", b"abcdefghij"]).unwrap();
    journal.snapshot().unwrap();
    let before = journal.statistics().unwrap();
    assert_eq!(before.logical_bytes, 20);
    assert_eq!(
        before.physical_bytes,
        Some(
            std::fs::metadata(dir.root().join(JOURNAL_FILE))
                .unwrap()
                .len()
        )
    );
    assert!(before.physical_bytes.unwrap() >= before.logical_bytes);

    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 0,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    match outcome {
        RetentionOutcome::Advanced {
            logical_bytes_after,
            physical_bytes,
            ..
        } => {
            assert_eq!(logical_bytes_after, 0);
            assert_eq!(
                physical_bytes,
                Some(
                    std::fs::metadata(dir.root().join(JOURNAL_FILE))
                        .unwrap()
                        .len()
                )
            );
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(journal.statistics().unwrap().logical_bytes, 0);
}

#[test]
#[cfg(unix)]
fn the_size_is_measured_on_the_opened_file_not_on_the_nominal_path() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    journal.append(&[b"aaaa", b"bbbb"]).unwrap();
    journal.snapshot().unwrap();
    let opened = journal.statistics().unwrap().physical_bytes.unwrap();
    assert!(opened > 1);

    // Il nome non identifica più il journal aperto: al suo posto c'è un file
    // di un byte. La misura deve restare quella del file ancora aperto.
    let nominal = dir.root().join(JOURNAL_FILE);
    std::fs::rename(&nominal, dir.root().join("journal.redb.spostato")).unwrap();
    std::fs::write(&nominal, b"x").unwrap();
    assert_eq!(std::fs::metadata(&nominal).unwrap().len(), 1);

    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 0,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    let measured = match outcome {
        RetentionOutcome::Advanced {
            to_floor: 2,
            physical_bytes: Some(measured),
            ..
        } => measured,
        other => panic!("esito inatteso: {other:?}"),
    };
    assert!(
        measured > 1,
        "la misura ha seguito il percorso nominale: {measured}"
    );
    let stats = journal.statistics().unwrap();
    assert_eq!(stats.floor, 2);
    assert!(stats.physical_bytes.unwrap() > 1);
}

#[test]
fn a_measurement_failure_does_not_hide_a_committed_retention() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    journal.append(&[b"aaaa", b"bbbb"]).unwrap();
    journal.snapshot().unwrap();

    fault::fail_measurement(true);
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 0,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    fault::fail_measurement(false);

    assert!(
        matches!(
            outcome,
            RetentionOutcome::Advanced {
                from_floor: 0,
                to_floor: 2,
                removed: 2,
                physical_bytes: None,
                ..
            }
        ),
        "esito inatteso: {outcome:?}"
    );
    let stats = journal.statistics().unwrap();
    assert_eq!(
        (stats.floor, stats.high_water, stats.logical_bytes),
        (2, 2, 0)
    );
    assert!(stats.physical_bytes.is_some());
}

/// Prova il batch che deve estendere il file con un limite coincidente con la
/// lunghezza attuale e restituisce la lunghezza che avrebbe raggiunto.
fn refused_growth(journal: Journal, limit: u64, batch: &[&[u8]]) -> (Journal, u64) {
    let mut journal = journal
        .with_budget(JournalBudget {
            max_file_bytes: limit + 1,
            maintenance_reserve: 1,
        })
        .unwrap();
    match journal.append(batch).unwrap_err() {
        JournalError::CapacityExhausted {
            limit: actual,
            file_bytes,
        } => {
            assert_eq!(actual, limit);
            assert!(file_bytes > limit, "{file_bytes} <= {limit}");
            (journal, file_bytes)
        }
        other => panic!("esito inatteso: {other:?}"),
    }
}

#[test]
fn an_invalid_budget_is_refused_without_touching_the_file() {
    let dir = Scratch::new();
    let (journal, _) = Journal::open(dir.shared()).unwrap();
    let bytes = journal.statistics().unwrap().physical_bytes.unwrap();
    drop(journal);

    for budget in [
        JournalBudget {
            max_file_bytes: 0,
            maintenance_reserve: 0,
        },
        JournalBudget {
            max_file_bytes: bytes,
            maintenance_reserve: bytes,
        },
        JournalBudget {
            max_file_bytes: 10,
            maintenance_reserve: 11,
        },
    ] {
        let (journal, _) = Journal::open(dir.shared()).unwrap();
        let before = std::fs::metadata(dir.root().join(JOURNAL_FILE))
            .unwrap()
            .len();
        let error = journal.with_budget(budget).expect_err("budget rifiutato");
        assert!(
            matches!(error, JournalError::InvalidBudget { .. }),
            "{error:?}"
        );
        assert_eq!(
            std::fs::metadata(dir.root().join(JOURNAL_FILE))
                .unwrap()
                .len(),
            before
        );
    }
}

#[test]
fn an_append_that_would_extend_the_file_past_the_budget_is_refused_and_leaves_no_trace() {
    let dir = Scratch::new();
    let (journal, _) = Journal::open(dir.shared()).unwrap();
    let baseline = journal.statistics().unwrap();
    let bytes = baseline.physical_bytes.unwrap();
    let mut journal = journal
        .with_budget(JournalBudget {
            max_file_bytes: bytes + 1,
            maintenance_reserve: 1,
        })
        .unwrap();

    let payload = [0x5au8; 64 * 1024];
    let batch = [payload.as_slice(); 16];
    // Il rifiuto è deterministico: nessun ritentativo automatico e la stessa
    // istanza resta nello stato committato, senza crescere oltre il file.
    for _ in 0..2 {
        match journal.append(&batch).unwrap_err() {
            JournalError::CapacityExhausted { limit, file_bytes } => {
                assert_eq!(limit, bytes);
                assert!(file_bytes > bytes);
            }
            other => panic!("esito inatteso: {other:?}"),
        }
        assert_eq!(journal.statistics().unwrap(), baseline);
        assert_eq!(journal.statistics().unwrap().physical_bytes, Some(bytes));
    }

    // L'istanza non è avvelenata: un budget che ammette la crescita accetta.
    let mut journal = journal
        .with_budget(JournalBudget {
            max_file_bytes: 64 * 1024 * 1024,
            maintenance_reserve: 0,
        })
        .unwrap();
    assert_eq!(journal.append(&[b"after"]).unwrap(), 1..=1);
    assert_eq!(journal.read_after(0, 1).unwrap(), vec![event(1, b"after")]);
}

#[test]
fn a_failed_rollback_requires_reopen_instead_of_declaring_capacity_exhausted() {
    let dir = Scratch::new();
    let (journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    let bytes = journal.statistics().unwrap().physical_bytes.unwrap();
    let mut journal = journal
        .with_budget(JournalBudget {
            max_file_bytes: bytes + 1,
            maintenance_reserve: 1,
        })
        .unwrap();
    let payload = [0x42u8; 64 * 1024];
    let batch = [payload.as_slice(); 16];

    fault::fail_abort(true);
    let error = journal.append(&batch).unwrap_err();
    fault::fail_abort(false);

    match error {
        JournalError::RollbackFailed {
            limit,
            file_bytes,
            detail,
        } => {
            assert_eq!(limit, bytes);
            assert!(file_bytes > bytes, "{file_bytes} <= {bytes}");
            assert!(!detail.is_empty());
        }
        other => panic!("esito inatteso: {other:?}"),
    }

    // L'annullamento non è riuscito: l'istanza non dichiara capacità esaurita
    // e non accetta altri batch, ma richiede la riapertura.
    assert!(matches!(
        journal.append(&[b"later"]).unwrap_err(),
        JournalError::RecoveryRequired
    ));
    assert!(matches!(
        journal.statistics().unwrap_err(),
        JournalError::RecoveryRequired
    ));
    drop(journal);

    // La riapertura riparte dallo stato committato: nessun evento e nessuna
    // sequenza del batch rifiutato sono visibili.
    let mut journal = journal_reopened(&dir, epoch);
    let stats = journal.statistics().unwrap();
    assert_eq!((stats.floor, stats.high_water, stats.events), (0, 0, 0));
    assert_eq!(journal.append(&[b"fresh"]).unwrap(), 1..=1);
}

#[test]
fn the_append_limit_is_inclusive_and_one_byte_below_refuses_the_same_growth() {
    let payload = [0x77u8; 8 * 1024];
    let batch = [payload.as_slice(); 16];

    let probe = Scratch::new();
    let (journal, _) = Journal::open(probe.shared()).unwrap();
    let bytes0 = journal.statistics().unwrap().physical_bytes.unwrap();
    let (journal, grown) = refused_growth(journal, bytes0, &batch);
    assert_eq!(journal.statistics().unwrap().physical_bytes, Some(bytes0));
    drop(journal);

    // Al limite: la lunghezza che la transazione raggiunge coincide con la
    // soglia e la scrittura è ammessa. La lunghezza dopo il commit può essere
    // minore, perché redb può ridurre la coda libera.
    let at = Scratch::new();
    let (journal, _) = Journal::open(at.shared()).unwrap();
    let mut journal = journal
        .with_budget(JournalBudget {
            max_file_bytes: grown,
            maintenance_reserve: 0,
        })
        .unwrap();
    assert_eq!(journal.append(&batch).unwrap(), 1..=16);
    let stats = journal.statistics().unwrap();
    assert_eq!(stats.high_water, 16);
    assert!(stats.physical_bytes.unwrap() <= grown);

    // Un byte sotto: la stessa crescita è rifiutata e il file resta quello
    // iniziale. La soglia è preventiva rispetto alla lunghezza finale, che
    // redb avrebbe potuto ridurre al commit.
    let below = Scratch::new();
    let (journal, _) = Journal::open(below.shared()).unwrap();
    let mut journal = journal
        .with_budget(JournalBudget {
            max_file_bytes: grown - 1,
            maintenance_reserve: 0,
        })
        .unwrap();
    match journal.append(&batch).unwrap_err() {
        JournalError::CapacityExhausted { limit, file_bytes } => {
            assert_eq!(limit, grown - 1);
            assert_eq!(file_bytes, grown);
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(journal.statistics().unwrap().physical_bytes, Some(bytes0));
}

#[test]
fn a_refused_append_leaves_room_for_snapshot_retention_and_resumed_appends() {
    let dir = Scratch::new();
    let (journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    let bytes0 = journal.statistics().unwrap().physical_bytes.unwrap();
    let payload = [0x33u8; 8 * 1024];
    let batch = [payload.as_slice(); 16];

    // La crescita che il primo batch raggiungerebbe è la soglia degli append:
    // con quella soglia il batch entra, il secondo che deve crescere no.
    let (journal, grown) = refused_growth(journal, bytes0, &batch);
    let mut journal = journal
        .with_budget(JournalBudget {
            max_file_bytes: grown,
            maintenance_reserve: 0,
        })
        .unwrap();
    assert_eq!(journal.append(&batch).unwrap(), 1..=16);
    let allocated_before = journal.allocated_bytes().unwrap();
    let (journal, second) = refused_growth(journal, grown, &batch);
    drop(journal);

    // Senza snapshot la retention non può rimuovere il prefisso.
    let mut journal = journal_reopened(&dir, epoch);
    let blocked = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 0,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(
        matches!(
            blocked,
            RetentionOutcome::Blocked {
                snapshot_through: None,
                ..
            }
        ),
        "esito inatteso: {blocked:?}"
    );
    assert_eq!(journal.statistics().unwrap().events, 16);

    // La riserva serve alla manutenzione: con il limite pieno lo snapshot e la
    // retention possono estendere il file oltre la soglia degli append.
    let mut journal = journal
        .with_budget(JournalBudget {
            max_file_bytes: second,
            maintenance_reserve: second - grown,
        })
        .unwrap();
    journal.snapshot().unwrap();
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 0,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(
        matches!(
            outcome,
            RetentionOutcome::Advanced {
                from_floor: 0,
                to_floor: 16,
                removed: 16,
                ..
            }
        ),
        "esito inatteso: {outcome:?}"
    );

    // Spazio riutilizzabile dentro redb: le pagine allocate calano mentre il
    // file non è necessariamente più corto. È il riuso, non un troncamento.
    let allocated_after = journal.allocated_bytes().unwrap();
    let physical_after = journal.statistics().unwrap().physical_bytes.unwrap();
    assert!(
        allocated_after < allocated_before,
        "pagine allocate {allocated_after} >= {allocated_before}"
    );
    assert!(physical_after <= second);
    let stats = journal.statistics().unwrap();
    assert_eq!(
        (stats.epoch, stats.floor, stats.high_water, stats.events),
        (epoch, 16, 16, 0)
    );

    // Gli append riprendono senza superare la soglia: la nuova sequenza segue
    // il floor e non riusa numeri rimossi.
    assert_eq!(journal.append(&[b"resume"]).unwrap(), 17..=17);
    let stats = journal.statistics().unwrap();
    assert_eq!((stats.floor, stats.high_water, stats.events), (16, 17, 1));
    assert_eq!(
        journal
            .read_from(Position::new(epoch, 16), 2)
            .unwrap()
            .events,
        vec![event(17, b"resume")]
    );
    assert!(stats.physical_bytes.unwrap() <= second);
}

/// Riapre lo stesso journal con il budget predefinito, conservando epoch e
/// stato: le prove non devono dipendere dall'ordine delle istanze.
fn journal_reopened(dir: &Scratch, epoch: u64) -> Journal {
    let (journal, _) = Journal::open(dir.shared()).unwrap();
    assert_eq!(journal.epoch().unwrap(), epoch);
    journal
}

#[test]
fn page_reads_are_stable_across_later_appends() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.shared()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"one"]).unwrap();
    let first = journal.read_from(Position::new(epoch, 0), 1).unwrap();
    journal.append(&[b"two"]).unwrap();
    let second = journal
        .read_from(Position::new(epoch, first.high_water), 1)
        .unwrap();
    assert_eq!(first.events, vec![event(1, b"one")]);
    assert_eq!(first.high_water, 1);
    assert_eq!(second.events, vec![event(2, b"two")]);
    assert_eq!(second.high_water, 2);
}
