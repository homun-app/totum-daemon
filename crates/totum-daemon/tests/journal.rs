#![cfg(any(target_os = "macos", target_os = "linux", windows))]

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use totum_daemon::config::{CoherenceInput, ComponentState, Store};
use totum_daemon::journal::{
    Event, JOURNAL_FILE, Journal, JournalError, Position, RetentionLimits, RetentionOutcome,
    StartupKind,
};
use totum_daemon::platform::{self, LockedRoot, PlatformError, SharedRoot};

const COHERENT: CoherenceInput = CoherenceInput {
    identity: ComponentState::Initial,
    registry: ComponentState::Initial,
};

/// Coordinazione dell'harness fra avvio dei processi figli e transizioni del
/// lock, come in `tests/lock.rs`.
///
/// Un figlio forchettato eredita i descrittori aperti del padre finché non
/// esegue, e `flock` è legato alla open file description: un riferimento
/// ereditato tiene il lock anche dopo che il padre ha chiuso il proprio
/// descrittore. Il prodotto non crea processi; la finestra esiste soltanto qui,
/// dove più test dello stesso binario forkano figli che a loro volta aprono
/// journal. Chi rilascia e riacquisisce un lock prende questo mutex, che ogni
/// `spawn` tiene finché `exec` non è avvenuto: nessun avvio può cadere fra il
/// rilascio e la riacquisizione. Non è un'attesa e non disattiva il
/// parallelismo delle prove.
static FORKS: Mutex<()> = Mutex::new(());

fn fork_window() -> MutexGuard<'static, ()> {
    FORKS.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("target/journal-tests");
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

    fn lock(&self) -> LockedRoot {
        platform::lock(platform::prepare_root(&self.root()).unwrap()).unwrap()
    }

    fn shared(&self) -> SharedRoot {
        self.lock().share()
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        let root = platform::prepare_root(&self.root()).unwrap();
        let mut file = root.create_file(OsStr::new(name)).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
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

fn journal_temporaries(directory: &Path) -> Vec<String> {
    names(directory)
        .into_iter()
        .filter(|name| name.starts_with(".journal.") && name.ends_with(".tmp"))
        .collect()
}

struct Reap(std::process::Child);

impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Avvia un test ignorato dello stesso binario come processo separato.
///
/// Il mutex delle fork è tenuto fino al ritorno di `spawn`, che attende
/// `exec`: nessun avvio può cadere fra il rilascio e la riacquisizione del
/// lock di un altro test.
fn spawn_child(test: &str, dir: &Scratch) -> Reap {
    let _forks = fork_window();
    Reap(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", test])
            .env("TOTUM_TEST_JOURNAL_ROOT", dir.root())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    )
}

/// Attende la barriera scritta dal figlio, senza attesa attiva sul lock.
fn wait_ready(dir: &Scratch, child: &mut Reap) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "figlio terminato prima della barriera"
        );
        if std::fs::read(dir.0.join("ready")).is_ok_and(|bytes| bytes == b"ok") {
            return;
        }
        assert!(Instant::now() < deadline, "timeout attendendo il commit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn journal_persists_batches_and_pages_across_reopen() {
    let dir = Scratch::new();
    let (mut journal, startup) = Journal::open(dir.lock()).unwrap();
    assert!(matches!(startup.kind, StartupKind::FirstRun { .. }));
    assert!(startup.discarded_temporaries.is_empty());
    match &startup.kind {
        StartupKind::FirstRun { publication } => {
            // Su Windows la voce di directory non è sincronizzabile: l'esito
            // è pubblicato ma non dichiarato durevole.
            assert_eq!(publication.durable(), cfg!(unix), "{publication:?}");
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert!(dir.root().join(JOURNAL_FILE).is_file());
    assert!(journal_temporaries(&dir.root()).is_empty());

    assert!(journal.read_after(0, 1).unwrap().is_empty());
    assert_eq!(journal.append(&[b"first", b"second"]).unwrap(), 1..=2);
    assert!(matches!(
        journal.append(&[b""]),
        Err(JournalError::InvalidBatch)
    ));
    assert!(matches!(
        platform::lock(platform::prepare_root(&dir.root()).unwrap()),
        Err(PlatformError::AlreadyRunning { .. })
    ));
    let (mut journal, startup) = {
        let _forks = fork_window();
        drop(journal);
        Journal::open(dir.lock()).unwrap()
    };
    assert!(matches!(startup.kind, StartupKind::Existing { .. }));
    assert!(startup.discarded_temporaries.is_empty());
    let events = journal.read_after(1, 1).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sequence, 2);
    assert_eq!(events[0].payload, b"second");
    assert_eq!(journal.append(&[b"third"]).unwrap(), 3..=3);
}

#[test]
fn a_second_journal_on_the_same_root_is_refused_before_touching_the_file() {
    let dir = Scratch::new();
    let shared = dir.shared();
    let (mut first, _) = Journal::open(shared.clone()).unwrap();
    assert_eq!(first.append(&[b"kept"]).unwrap(), 1..=1);
    let path = dir.root().join(JOURNAL_FILE);
    let before = std::fs::read(&path).unwrap();

    for _ in 0..2 {
        match Journal::open(shared.clone()) {
            Err(JournalError::AlreadyOpen { .. }) => {}
            Err(other) => panic!("errore inatteso: {other:?}"),
            Ok(_) => panic!("una seconda apertura sulla stessa radice non deve riuscire"),
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "il rifiuto avviene prima di qualunque accesso a redb"
        );
    }

    let (mut second, startup) = {
        let _forks = fork_window();
        drop(first);
        Journal::open(shared).unwrap()
    };
    assert!(matches!(startup.kind, StartupKind::Existing { .. }));
    let events = second.read_after(0, 1).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload, b"kept");
    assert_eq!(second.append(&[b"next"]).unwrap(), 2..=2);
}

#[test]
fn journal_refuses_a_hard_link_without_changing_the_target() {
    let dir = Scratch::new();
    let source = Scratch::new();
    drop(Journal::open(source.lock()).unwrap());
    let victim = source.root().join(JOURNAL_FILE);
    let before = std::fs::read(&victim).unwrap();
    let locked = dir.lock();
    std::fs::hard_link(&victim, dir.root().join(JOURNAL_FILE)).unwrap();
    assert!(matches!(
        Journal::open(locked),
        Err(JournalError::Platform(_))
    ));
    assert_eq!(std::fs::read(victim).unwrap(), before);
}

#[test]
fn an_interrupted_bootstrap_residue_is_discarded_not_published() {
    let dir = Scratch::new();
    dir.write(".journal.4242.0.tmp", b"parziale");
    dir.write("notes.txt", b"nota");
    // Nomi simili ma fuori dal namespace: non vengono toccati.
    dir.write("x.journal.1.0.tmp", b"estraneo");
    dir.write(".journal.1.0.tmp.bak", b"copia");

    let (mut journal, startup) = Journal::open(dir.lock()).unwrap();
    match &startup.kind {
        StartupKind::InterruptedBootstrap { publication } => {
            assert_eq!(publication.durable(), cfg!(unix), "{publication:?}");
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(
        startup.discarded_temporaries,
        vec![std::ffi::OsString::from(".journal.4242.0.tmp")]
    );
    assert!(dir.root().join(JOURNAL_FILE).is_file());
    assert_eq!(
        std::fs::read(dir.root().join("notes.txt")).unwrap(),
        b"nota"
    );
    assert_eq!(
        std::fs::read(dir.root().join("x.journal.1.0.tmp")).unwrap(),
        b"estraneo"
    );
    assert_eq!(
        std::fs::read(dir.root().join(".journal.1.0.tmp.bak")).unwrap(),
        b"copia"
    );
    assert!(journal_temporaries(&dir.root()).is_empty());
    assert_eq!(journal.append(&[b"one"]).unwrap(), 1..=1);
}

#[test]
fn a_corrupt_journal_is_refused_without_safe_overwrite() {
    let dir = Scratch::new();
    let corrupt = b"non e un journal redb";
    dir.write(JOURNAL_FILE, corrupt);

    match Journal::open(dir.lock()) {
        Err(JournalError::Corrupt { .. }) => {}
        Err(other) => panic!("errore inatteso: {other:?}"),
        Ok(_) => panic!("un journal corrotto non deve essere aperto"),
    }
    assert_eq!(
        std::fs::read(dir.root().join(JOURNAL_FILE)).unwrap(),
        corrupt,
        "il principale corrotto non viene troncato né sostituito"
    );
    assert!(journal_temporaries(&dir.root()).is_empty());
}

#[test]
fn an_empty_journal_does_not_authorize_a_new_initialization() {
    let dir = Scratch::new();
    dir.write(JOURNAL_FILE, b"");

    match Journal::open(dir.lock()) {
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
    assert!(journal_temporaries(&dir.root()).is_empty());
}

#[test]
fn the_lock_is_shared_by_config_and_journal_until_the_last_user() {
    let dir = Scratch::new();
    let shared = dir.shared();
    let (journal, _) = Journal::open(shared.clone()).unwrap();
    let (store, _) = Store::open_shared(shared, COHERENT).unwrap();

    assert_eq!(
        probe(&dir),
        Probe::Refused,
        "entrambi gli utilizzatori vivi"
    );
    drop(journal);
    assert_eq!(
        probe(&dir),
        Probe::Refused,
        "lo store di configurazione trattiene ancora il lock"
    );
    drop(store);
    assert_eq!(
        probe(&dir),
        Probe::Acquired,
        "l'ultimo utilizzatore ha rilasciato il lock"
    );
}

#[derive(Debug, PartialEq, Eq)]
enum Probe {
    Acquired,
    Refused,
    Error(i32),
}

fn probe(dir: &Scratch) -> Probe {
    // `status` include `spawn`, che attende `exec`: tenendo il mutex fino al
    // suo ritorno, nessun avvio di figlio può cadere fra il rilascio e la
    // riacquisizione di un altro test.
    let _forks = fork_window();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "lock_probe_child"])
        .env("TOTUM_TEST_LOCK_ROOT", dir.root())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    match status.code() {
        Some(0) => Probe::Acquired,
        Some(3) => Probe::Refused,
        Some(code) => Probe::Error(code),
        None => Probe::Error(-1),
    }
}

#[test]
#[ignore = "eseguito soltanto dal test padre come secondo processo"]
fn lock_probe_child() {
    let Ok(root) = std::env::var("TOTUM_TEST_LOCK_ROOT") else {
        return;
    };
    let root = platform::prepare_root(Path::new(&root)).unwrap();
    let code = match platform::lock(root) {
        Ok(_) => 0,
        Err(PlatformError::AlreadyRunning { .. }) => 3,
        Err(_) => 4,
    };
    std::process::exit(code);
}

#[test]
#[ignore = "eseguito soltanto dal test padre"]
fn journal_crash_child() {
    let Ok(root) = std::env::var("TOTUM_TEST_JOURNAL_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let locked = platform::lock(platform::prepare_root(root).unwrap()).unwrap();
    let (mut journal, _) = Journal::open(locked).unwrap();
    assert_eq!(journal.append(&[b"one", b"two"]).unwrap(), 1..=2);
    std::fs::write(root.parent().unwrap().join("ready"), b"ok").unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
#[ignore = "eseguito soltanto dal test padre"]
fn journal_retention_child() {
    let Ok(root) = std::env::var("TOTUM_TEST_JOURNAL_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let locked = platform::lock(platform::prepare_root(root).unwrap()).unwrap();
    let (mut journal, _) = Journal::open(locked).unwrap();
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 0,
                max_age: Duration::MAX,
            },
            SystemTime::now(),
        )
        .unwrap();
    assert!(matches!(outcome, RetentionOutcome::Advanced { .. }));
    std::fs::write(root.parent().unwrap().join("ready"), b"ok").unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn acknowledged_batch_survives_process_termination() {
    let dir = Scratch::new();
    let mut child = spawn_child("journal_crash_child", &dir);
    wait_ready(&dir, &mut child);
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    let (mut journal, startup) = Journal::open(dir.lock()).unwrap();
    // La terminazione non ha chiuso il database: il motore ricostruisce il
    // proprio stato e la riapertura lo dichiara, senza reinterpretare i dati.
    assert!(
        matches!(startup.kind, StartupKind::Existing { repaired: true }),
        "esito inatteso: {:?}",
        startup.kind
    );
    assert_eq!(
        journal.read_after(0, 2).unwrap(),
        [
            Event {
                sequence: 1,
                payload: b"one".to_vec()
            },
            Event {
                sequence: 2,
                payload: b"two".to_vec()
            },
        ]
    );
    assert_eq!(journal.append(&[b"three"]).unwrap(), 3..=3);
}

#[test]
fn the_retention_floor_survives_process_termination() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.lock()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"one", b"two", b"three"]).unwrap();
    journal.snapshot().unwrap();
    drop(journal);

    let mut child = spawn_child("journal_retention_child", &dir);
    wait_ready(&dir, &mut child);
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());

    let (mut journal, startup) = Journal::open(dir.lock()).unwrap();
    assert!(
        matches!(startup.kind, StartupKind::Existing { repaired: true }),
        "esito inatteso: {:?}",
        startup.kind
    );
    assert_eq!(journal.epoch().unwrap(), epoch);
    let stats = journal.statistics().unwrap();
    assert_eq!((stats.floor, stats.high_water, stats.events), (3, 3, 0));
    assert!(matches!(
        journal.read_from(Position::new(epoch, 0), 1),
        Err(JournalError::RetentionExpired { floor: 3, .. })
    ));
    assert_eq!(journal.append(&[b"four"]).unwrap(), 4..=4);
}

#[test]
fn positions_epoch_and_floor_survive_reopen() {
    let dir = Scratch::new();
    let (mut journal, _) = Journal::open(dir.lock()).unwrap();
    let epoch = journal.epoch().unwrap();
    journal.append(&[b"a", b"b", b"c"]).unwrap();
    journal.snapshot().unwrap();
    let outcome = journal
        .retain(
            RetentionLimits {
                max_logical_bytes: 2,
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
    let (mut journal, _) = {
        let _forks = fork_window();
        drop(journal);
        Journal::open(dir.lock()).unwrap()
    };
    assert_eq!(journal.epoch().unwrap(), epoch);
    assert_eq!(journal.statistics().unwrap().floor, 1);
    assert!(matches!(
        journal.read_from(Position::new(epoch, 0), 1),
        Err(JournalError::RetentionExpired { floor: 1, .. })
    ));
    assert_eq!(journal.append(&[b"d"]).unwrap(), 4..=4);

    let next = journal.reset().unwrap();
    assert_ne!(next, epoch);
    assert!(matches!(
        journal.read_from(Position::new(epoch, 3), 1),
        Err(JournalError::EpochChanged { expected, found })
            if expected == next && found == epoch
    ));
    assert_eq!(journal.append(&[b"fresh"]).unwrap(), 1..=1);
    let (journal, _) = {
        let _forks = fork_window();
        drop(journal);
        Journal::open(dir.lock()).unwrap()
    };
    assert_eq!(journal.epoch().unwrap(), next);
    assert_eq!(
        journal
            .read_from(Position::new(next, 0), 16)
            .unwrap()
            .events,
        [Event {
            sequence: 1,
            payload: b"fresh".to_vec()
        }]
    );
}
