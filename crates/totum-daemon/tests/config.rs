//! Prove d'integrazione della classificazione della configurazione.
//!
//! Ogni prova usa una radice temporanea creata dalle primitive verificate del
//! modulo `platform`; nessun profilo o configurazione reale viene toccata. Le
//! prove verificano anche che la classificazione non cambi contenuto o presenza
//! dei file. Le prove sono comuni a Unix e Windows, salvo quelle che richiedono
//! primitive di un solo sistema (modalità POSIX, collegamenti, accesso negato).

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

use totum_daemon::config::{
    self, BackupState, CONFIG_BACKUP_FILE, CONFIG_FILE, CoherenceInput, Commit, Component,
    ComponentState, ConfigError, ConfigV1, Decision, GenerationFailure, MigrationSource, Startup,
    StartupKind, Store,
};
use totum_daemon::platform::{self, PlatformError, VerifiedRoot};

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
            "{label}-{}-{}",
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

fn classify(root: &VerifiedRoot) -> Decision {
    config::classify(root, COHERENT)
}

fn classify_with(root: &VerifiedRoot, coherence: CoherenceInput) -> Decision {
    config::classify(root, coherence)
}

fn write(root: &VerifiedRoot, name: &str, contents: &[u8]) {
    let mut file = root.create_file(OsStr::new(name)).expect("file creabile");
    file.write_all(contents).expect("contenuto scritto");
    file.sync_all().expect("contenuto sincronizzato");
}

fn valid(revision: u64) -> Vec<u8> {
    format!("{{\"schemaVersion\":1,\"revision\":{revision}}}").into_bytes()
}

fn normal(decision: Decision) -> (ConfigV1, BackupState) {
    match decision {
        Decision::Normal { config, backup } => (config, backup),
        other => panic!("decisione inattesa: {other:?}"),
    }
}

fn refused(decision: Decision) -> ConfigError {
    match decision {
        Decision::Refused(error) => error,
        other => panic!("decisione inattesa: {other:?}"),
    }
}

/// Nome e contenuto di ogni voce della radice, ordinati; le modalità POSIX
/// sono controllate dalle prove specifiche di Unix.
fn snapshot(directory: &Path) -> Vec<(String, Vec<u8>)> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).expect("directory leggibile") {
        let entry = entry.expect("voce leggibile");
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).expect("metadati disponibili");
        let contents = if metadata.is_file() {
            fs::read(&path).expect("file leggibile")
        } else {
            Vec::new()
        };
        entries.push((entry.file_name().to_string_lossy().into_owned(), contents));
    }
    entries.sort();
    entries
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("metadati disponibili")
        .permissions()
        .mode()
        & 0o7777
}

#[test]
fn main_is_loaded_and_older_backup_stays_reserved() {
    let scratch = Scratch::new("older-backup");
    let root = scratch.root();
    write(&root, CONFIG_FILE, &valid(5));
    write(&root, CONFIG_BACKUP_FILE, &valid(4));

    let (config, backup) = normal(classify(&root));
    assert_eq!(config.schema_version(), 1);
    assert_eq!(config.revision(), 5);
    assert_eq!(backup, BackupState::Available { revision: 4 });
}

#[test]
fn equal_revisions_are_normal() {
    let scratch = Scratch::new("equal-revision");
    let root = scratch.root();
    write(&root, CONFIG_FILE, &valid(5));
    write(&root, CONFIG_BACKUP_FILE, &valid(5));

    let (config, backup) = normal(classify(&root));
    assert_eq!(config.revision(), 5);
    assert_eq!(backup, BackupState::Available { revision: 5 });
}

#[test]
fn valid_main_without_backup_is_normal() {
    let scratch = Scratch::new("no-backup");
    let root = scratch.root();
    write(&root, CONFIG_FILE, &valid(3));

    let (config, backup) = normal(classify(&root));
    assert_eq!(config.revision(), 3);
    assert_eq!(backup, BackupState::Absent);
}

#[test]
fn corrupt_backup_is_normal_and_signalled_without_repair() {
    let scratch = Scratch::new("corrupt-backup");
    let root = scratch.root();
    write(&root, CONFIG_FILE, &valid(3));
    write(&root, CONFIG_BACKUP_FILE, b"{\"schemaVersion\":1,");
    let before = snapshot(root.path());

    let (config, backup) = normal(classify(&root));
    assert_eq!(config.revision(), 3);
    assert!(matches!(backup, BackupState::Corrupt { .. }));
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn backup_newer_than_main_is_refused() {
    let scratch = Scratch::new("regression");
    let root = scratch.root();
    write(&root, CONFIG_FILE, &valid(4));
    write(&root, CONFIG_BACKUP_FILE, &valid(5));

    match refused(classify(&root)) {
        ConfigError::RevisionRegression { main, backup } => {
            assert_eq!(main, 4);
            assert_eq!(backup, 5);
        }
        other => panic!("errore inatteso: {other}"),
    }
}

#[test]
fn corrupt_main_with_valid_backup_requires_recovery() {
    let scratch = Scratch::new("recover-main");
    let root = scratch.root();
    write(&root, CONFIG_FILE, b"{\"schemaVersion\":1,\"revision\":");
    write(&root, CONFIG_BACKUP_FILE, &valid(4));
    let before = snapshot(root.path());

    match classify(&root) {
        Decision::RecoveryRequired { backup, main } => {
            assert_eq!(backup.revision(), 4);
            assert!(matches!(main, GenerationFailure::Corrupt { .. }));
        }
        other => panic!("decisione inattesa: {other:?}"),
    }
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn absent_main_with_valid_backup_requires_recovery() {
    let scratch = Scratch::new("recover-absent");
    let root = scratch.root();
    write(&root, CONFIG_BACKUP_FILE, &valid(4));

    match classify(&root) {
        Decision::RecoveryRequired { backup, main } => {
            assert_eq!(backup.revision(), 4);
            assert_eq!(main, GenerationFailure::Absent);
        }
        other => panic!("decisione inattesa: {other:?}"),
    }
}

#[test]
fn corrupt_generations_without_a_usable_backup_are_refused() {
    for (label, main, backup) in [
        ("corrupt-absent", Some(&b"not json"[..]), None),
        ("absent-corrupt", None, Some(&b"not json"[..])),
        ("both-corrupt", Some(&b"not json"[..]), Some(&b"{}\n"[..])),
    ] {
        let scratch = Scratch::new(label);
        let root = scratch.root();
        if let Some(contents) = main {
            write(&root, CONFIG_FILE, contents);
        }
        if let Some(contents) = backup {
            write(&root, CONFIG_BACKUP_FILE, contents);
        }
        assert!(
            matches!(
                refused(classify(&root)),
                ConfigError::NoValidGeneration { .. }
            ),
            "{label}"
        );
    }
}

#[test]
fn future_schema_blocks_the_main_and_the_backup() {
    let scratch = Scratch::new("future-main");
    let root = scratch.root();
    write(
        &root,
        CONFIG_FILE,
        br#"{"schemaVersion":2,"revision":5,"adapter":{"id":"x"}}"#,
    );
    write(
        &root,
        CONFIG_BACKUP_FILE,
        br#"{"schemaVersion":2,"revision":4}"#,
    );
    assert!(matches!(
        refused(classify(&root)),
        ConfigError::TooNew {
            schema_version: 2,
            ..
        }
    ));

    let scratch = Scratch::new("future-backup");
    let root = scratch.root();
    write(&root, CONFIG_FILE, &valid(5));
    write(
        &root,
        CONFIG_BACKUP_FILE,
        br#"{"schemaVersion":2,"revision":4}"#,
    );
    assert!(matches!(
        refused(classify(&root)),
        ConfigError::TooNew {
            schema_version: 2,
            ..
        }
    ));

    // Uno schema futuro nel backup blocca anche una migrazione del principale.
    let scratch = Scratch::new("future-backup-v0-main");
    let root = scratch.root();
    write(&root, CONFIG_FILE, br#"{"schemaVersion":0}"#);
    write(
        &root,
        CONFIG_BACKUP_FILE,
        br#"{"schemaVersion":2,"revision":4}"#,
    );
    assert!(matches!(
        refused(classify(&root)),
        ConfigError::TooNew {
            schema_version: 2,
            ..
        }
    ));
}

#[test]
fn older_schema_requires_migration_and_an_older_backup_is_reserved() {
    let scratch = Scratch::new("older-main");
    let root = scratch.root();
    write(&root, CONFIG_FILE, br#"{"schemaVersion":0}"#);
    match classify(&root) {
        Decision::MigrationRequired {
            from_schema_version: 0,
            source: MigrationSource::Main,
            main_failure: None,
        } => {}
        other => panic!("decisione inattesa: {other:?}"),
    }

    // Il principale è valido: un backup con schema precedente resta di
    // riserva e non viene migrato al suo posto.
    let scratch = Scratch::new("older-backup");
    let root = scratch.root();
    write(&root, CONFIG_FILE, &valid(5));
    write(&root, CONFIG_BACKUP_FILE, br#"{"schemaVersion":0}"#);
    let (config, backup) = normal(classify(&root));
    assert_eq!(config.revision(), 5);
    assert_eq!(backup, BackupState::OlderSchema { schema_version: 0 });
}

#[test]
fn a_v0_main_with_a_v1_backup_is_a_schema_regression() {
    let scratch = Scratch::new("schema-regression");
    let root = scratch.root();
    write(&root, CONFIG_FILE, br#"{"schemaVersion":0}"#);
    write(&root, CONFIG_BACKUP_FILE, &valid(4));
    let before = snapshot(root.path());

    match refused(classify(&root)) {
        ConfigError::SchemaRegression { main: 0, backup: 1 } => {}
        other => panic!("errore inatteso: {other}"),
    }
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn invalid_revision_is_refused_and_not_recovered_from_the_backup() {
    for (label, contents) in [
        ("negative", &br#"{"schemaVersion":1,"revision":-1}"#[..]),
        ("string", &br#"{"schemaVersion":1,"revision":"4"}"#[..]),
        ("fraction", &br#"{"schemaVersion":1,"revision":4.5}"#[..]),
    ] {
        let scratch = Scratch::new(label);
        let root = scratch.root();
        write(&root, CONFIG_FILE, contents);
        write(&root, CONFIG_BACKUP_FILE, &valid(4));
        assert!(
            matches!(
                refused(classify(&root)),
                ConfigError::InvalidRevision { .. }
            ),
            "{label}"
        );
    }
}

#[test]
fn structurally_corrupt_documents_recover_from_the_backup() {
    for (label, contents) in [
        (
            "unknown-field",
            &br#"{"schemaVersion":1,"revision":9,"enabled":true}"#[..],
        ),
        (
            "duplicate-revision",
            &br#"{"schemaVersion":1,"revision":9,"revision":9}"#[..],
        ),
        ("missing-revision", &br#"{"schemaVersion":1}"#[..]),
        ("truncated", &br#"{"schemaVersion":1,"revision":"#[..]),
    ] {
        let scratch = Scratch::new(label);
        let root = scratch.root();
        write(&root, CONFIG_FILE, contents);
        write(&root, CONFIG_BACKUP_FILE, &valid(4));
        match classify(&root) {
            Decision::RecoveryRequired { backup, .. } => assert_eq!(backup.revision(), 4),
            other => panic!("{label}: decisione inattesa: {other:?}"),
        }
    }
}

#[test]
fn first_run_yields_initial_values_in_memory_only() {
    let scratch = Scratch::new("first-run");
    let root = scratch.root();

    match classify(&root) {
        Decision::FirstRun { config } => {
            assert_eq!(config.schema_version(), 1);
            assert_eq!(config.revision(), 0);
        }
        other => panic!("decisione inattesa: {other:?}"),
    }
    assert!(
        fs::read_dir(root.path())
            .expect("directory leggibile")
            .next()
            .is_none(),
        "nessun file creato dalla classificazione"
    );

    for (label, coherence) in [
        (
            "both-absent",
            CoherenceInput {
                identity: ComponentState::Absent,
                registry: ComponentState::Absent,
            },
        ),
        (
            "identity-present",
            CoherenceInput {
                identity: ComponentState::Initial,
                registry: ComponentState::Absent,
            },
        ),
        (
            "registry-present",
            CoherenceInput {
                identity: ComponentState::Absent,
                registry: ComponentState::Initial,
            },
        ),
    ] {
        let scratch = Scratch::new(label);
        let root = scratch.root();
        assert!(
            matches!(
                classify_with(&root, coherence),
                Decision::FirstRun { config } if config.revision() == 0
            ),
            "{label}"
        );
    }
}

#[test]
fn non_initial_or_unusable_components_are_refused() {
    let scratch = Scratch::new("non-initial");
    let root = scratch.root();
    for (label, coherence) in [
        (
            "identity-non-initial",
            CoherenceInput {
                identity: ComponentState::NonInitial,
                registry: ComponentState::Initial,
            },
        ),
        (
            "registry-unusable",
            CoherenceInput {
                identity: ComponentState::Initial,
                registry: ComponentState::Unusable,
            },
        ),
    ] {
        match refused(classify_with(&root, coherence)) {
            ConfigError::ComponentState { .. } => {}
            other => panic!("{label}: errore inatteso: {other}"),
        }
    }
}

#[test]
fn configuration_without_identity_or_registry_is_incoherent() {
    for (label, coherence, expected) in [
        (
            "no-identity",
            CoherenceInput {
                identity: ComponentState::Absent,
                registry: ComponentState::Initial,
            },
            Component::Identity,
        ),
        (
            "no-registry",
            CoherenceInput {
                identity: ComponentState::Initial,
                registry: ComponentState::Absent,
            },
            Component::Registry,
        ),
    ] {
        let scratch = Scratch::new(label);
        let root = scratch.root();
        write(&root, CONFIG_FILE, &valid(1));
        match refused(classify_with(&root, coherence)) {
            ConfigError::IncoherentState { component } => assert_eq!(component, expected),
            other => panic!("{label}: errore inatteso: {other}"),
        }
    }
}

#[test]
fn size_limit_is_enforced_at_the_boundary() {
    let scratch = Scratch::new("size-boundary");
    let root = scratch.root();
    let mut contents = valid(1);
    contents.resize(64 * 1024, b' ');
    write(&root, CONFIG_FILE, &contents);
    let (config, _) = normal(classify(&root));
    assert_eq!(config.revision(), 1);

    let scratch = Scratch::new("size-over");
    let root = scratch.root();
    let mut contents = valid(1);
    contents.resize(64 * 1024 + 1, b' ');
    write(&root, CONFIG_FILE, &contents);
    assert!(matches!(
        refused(classify(&root)),
        ConfigError::TooLarge { limit: 65536, .. }
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_configuration_is_refused_without_following_it() {
    let scratch = Scratch::new("symlink-config");
    let root = scratch.root();
    symlink(
        scratch.path.join("missing-target"),
        root.path().join(CONFIG_FILE),
    )
    .expect("collegamento creato");
    assert!(matches!(
        refused(classify(&root)),
        ConfigError::Protection(PlatformError::SymlinkRefused { .. })
    ));
}

#[cfg(unix)]
#[test]
fn too_permissive_configuration_is_refused_without_repair() {
    let scratch = Scratch::new("mode-config");
    let root = scratch.root();
    write(&root, CONFIG_FILE, &valid(1));
    let path = root.path().join(CONFIG_FILE);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("modalità impostata");

    assert!(matches!(
        refused(classify(&root)),
        ConfigError::Protection(PlatformError::WrongMode {
            expected: 0o600,
            actual: 0o644,
            ..
        })
    ));
    assert_eq!(mode(&path), 0o644, "la modalità non viene riparata");
}

#[cfg(unix)]
#[test]
fn unreadable_root_is_refused_without_repair() {
    let scratch = Scratch::new("denied-root");
    let root = scratch.root();
    write(&root, CONFIG_FILE, &valid(1));
    let path = root.path().to_path_buf();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("modalità impostata");

    let decision = refused(classify(&root));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("modalità ripristinata");

    match decision {
        ConfigError::Protection(PlatformError::Io { source, .. }) => assert_eq!(
            source.kind(),
            std::io::ErrorKind::PermissionDenied,
            "l'accesso negato resta un errore di sistema"
        ),
        other => panic!("errore inatteso: {other}"),
    }
}

#[test]
fn directory_in_place_of_configuration_is_refused() {
    let scratch = Scratch::new("config-directory");
    let root = scratch.root();
    fs::create_dir(root.path().join(CONFIG_FILE)).expect("directory creata");

    #[cfg(unix)]
    assert!(matches!(
        refused(classify(&root)),
        ConfigError::Protection(PlatformError::NotARegularFile { .. })
    ));
    // Windows apre la directory senza `FILE_FLAG_BACKUP_SEMANTICS` e riceve
    // `ERROR_ACCESS_DENIED`: resta un rifiuto di sistema, mai un'assenza.
    #[cfg(windows)]
    assert!(matches!(
        refused(classify(&root)),
        ConfigError::Protection(PlatformError::Io { .. })
    ));
}

#[test]
fn moved_or_replaced_root_is_refused() {
    let scratch = Scratch::new("root-moved");
    let nominal = scratch.path.join("totum");
    let root = platform::prepare_root(&nominal).expect("radice preparabile");

    let moved = scratch.path.join("moved");
    fs::rename(&nominal, &moved).expect("radice rinominata");
    fs::create_dir(&nominal).expect("radice sostituita");

    match refused(classify(&root)) {
        ConfigError::Protection(PlatformError::PathChanged { path }) => {
            assert_eq!(path, nominal, "il rifiuto nomina il percorso ancorato")
        }
        other => panic!("errore inatteso: {other}"),
    }
}

#[test]
fn temporary_files_are_ignored_and_preserved() {
    let scratch = Scratch::new("temporaries");
    let root = scratch.root();
    write(&root, ".config.1.0.tmp", &valid(9));
    let before = snapshot(root.path());

    assert!(matches!(classify(&root), Decision::FirstRun { .. }));
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn classification_leaves_every_read_only_case_unchanged() {
    let scratch = Scratch::new("unchanged");
    let root = scratch.root();
    write(&root, CONFIG_FILE, &valid(5));
    write(&root, CONFIG_BACKUP_FILE, &valid(4));
    let before = snapshot(root.path());
    let _ = classify(&root);
    assert_eq!(snapshot(root.path()), before);
    #[cfg(unix)]
    {
        assert_eq!(mode(&root.path().join(CONFIG_FILE)), 0o600);
        assert_eq!(mode(&root.path().join(CONFIG_BACKUP_FILE)), 0o600);
    }
}

// ---------------------------------------------------------------------------
// Scrittura, backup, recupero e salvataggi (D3 P4)
// ---------------------------------------------------------------------------

fn open(root: VerifiedRoot) -> (Store, Startup) {
    open_with(root, COHERENT)
}

fn open_with(root: VerifiedRoot, coherence: CoherenceInput) -> (Store, Startup) {
    let locked = platform::lock(root).expect("lock di istanza acquisito");
    Store::open(locked, coherence).expect("apertura riuscita")
}

/// Stato della radice senza `state.lock`, che l'acquisizione del lock crea e
/// non rimuove mai: serve ai rifiuti, dove non deve comparire altra scrittura.
fn state_snapshot(directory: &Path) -> Vec<(String, Vec<u8>)> {
    snapshot(directory)
        .into_iter()
        .filter(|(name, _)| name != platform::LOCK_FILE)
        .collect()
}

/// Generazione scritta dal modulo: la stessa intestazione v1, con l'a capo.
fn line(revision: u64) -> Vec<u8> {
    let mut bytes = valid(revision);
    bytes.push(b'\n');
    bytes
}

fn read_file(directory: &Path, name: &str) -> Vec<u8> {
    fs::read(directory.join(name)).expect("file leggibile")
}

/// Su Windows la voce di directory non viene sincronizzata: il commit è
/// riuscito ma la durabilità è dichiarata non confermata. Su Unix la
/// sincronizzazione di file e directory è richiesta e verificata.
fn assert_durable(commit: &Commit) {
    #[cfg(windows)]
    assert!(
        matches!(commit, Commit::CommittedNotDurable { .. }),
        "su Windows la durabilità non è dichiarata: {commit:?}"
    );
    #[cfg(not(windows))]
    assert!(
        matches!(commit, Commit::Committed { .. }),
        "durabilità attesa: {commit:?}"
    );
}

/// Revisione di una generazione completa, oppure `None` se il contenuto non è
/// esattamente una generazione serializzata.
fn complete_revision(bytes: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(bytes).ok()?;
    let body = text.strip_suffix('\n')?;
    let revision = body
        .strip_prefix("{\"schemaVersion\":1,\"revision\":")?
        .strip_suffix('}')?;
    if revision.is_empty() || !revision.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    revision.parse().ok()
}

#[test]
fn first_run_creates_revision_zero_and_reopening_writes_nothing() {
    let scratch = Scratch::new("store-first-run");
    let root = scratch.root();
    let directory = root.path().to_path_buf();

    let (store, startup) = open(root);
    match startup.kind {
        StartupKind::FirstRun { commit } => {
            assert_eq!(commit.revision(), 0);
            assert_durable(&commit);
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert!(startup.discarded_temporaries.is_empty());
    assert_eq!(store.config(), ConfigV1::initial());
    assert_eq!(read_file(&directory, CONFIG_FILE), line(0));
    assert!(!directory.join(CONFIG_BACKUP_FILE).exists());
    #[cfg(unix)]
    assert_eq!(mode(&directory.join(CONFIG_FILE)), 0o600);

    let before = snapshot(&directory);
    drop(store);

    let (store, startup) = open(scratch.root());
    assert!(matches!(
        startup.kind,
        StartupKind::Normal {
            backup: BackupState::Absent
        }
    ));
    assert_eq!(store.config().revision(), 0);
    assert_eq!(
        snapshot(&directory),
        before,
        "una riapertura normale non scrive"
    );
}

#[test]
fn successive_saves_advance_the_revision_and_fill_the_backup() {
    let scratch = Scratch::new("store-saves");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let (store, _) = open(root);

    let first = store.save().expect("primo salvataggio");
    assert_eq!(first.revision(), 1);
    assert_durable(&first);
    assert_eq!(store.config().revision(), 1);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(1));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), line(0));
    #[cfg(unix)]
    assert_eq!(mode(&directory.join(CONFIG_BACKUP_FILE)), 0o600);

    let second = store.save().expect("secondo salvataggio");
    assert_eq!(second.revision(), 2);
    assert_eq!(store.config().revision(), 2);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(2));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), line(1));

    drop(store);
    let (store, startup) = open(scratch.root());
    assert!(matches!(
        startup.kind,
        StartupKind::Normal {
            backup: BackupState::Available { revision: 1 }
        }
    ));
    assert_eq!(store.config().revision(), 2);
}

#[test]
fn concurrent_readers_see_only_complete_generations() {
    const SAVES: u64 = 20;

    let scratch = Scratch::new("store-concurrent");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let (store, _) = open(root);

    let stop = AtomicBool::new(false);
    let reads = AtomicU32::new(0);
    let reader_path = directory.join(CONFIG_FILE);
    std::thread::scope(|scope| {
        let (stop, reads, reader_path) = (&stop, &reads, &reader_path);
        scope.spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let bytes = fs::read(reader_path).expect("lettura riuscita");
                let revision = complete_revision(&bytes)
                    .unwrap_or_else(|| panic!("generazione incompleta letta: {bytes:?}"));
                assert!(revision <= SAVES, "revisione inattesa: {revision}");
                reads.fetch_add(1, Ordering::Relaxed);
            }
        });
        for _ in 0..SAVES {
            store.save().expect("salvataggio riuscito");
        }
        stop.store(true, Ordering::Relaxed);
    });

    assert!(reads.load(Ordering::Relaxed) > 0, "il lettore ha letto");
    assert_eq!(store.config().revision(), SAVES);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(SAVES));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), line(SAVES - 1));
}

#[test]
fn a_corrupt_main_is_recovered_and_preserved_with_the_backup_revision() {
    let scratch = Scratch::new("store-recover-corrupt");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let corrupt = b"{\"schemaVersion\":1,".to_vec();
    write(&root, CONFIG_FILE, &corrupt);
    write(&root, CONFIG_BACKUP_FILE, &valid(4));

    let (store, startup) = open(root);
    let preserved = match startup.kind {
        StartupKind::Recovered {
            backup,
            main_failure,
            preserved,
            commit,
        } => {
            assert_eq!(backup.revision(), 4);
            assert!(matches!(main_failure, GenerationFailure::Corrupt { .. }));
            assert_eq!(commit.revision(), 4);
            assert_durable(&commit);
            preserved.expect("copia del principale corrotto")
        }
        other => panic!("esito inatteso: {other:?}"),
    };

    assert_eq!(
        store.config().revision(),
        4,
        "la revisione recuperata è quella del backup"
    );
    assert_eq!(read_file(&directory, CONFIG_FILE), valid(4));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), valid(4));
    let preserved = preserved.to_string_lossy().into_owned();
    assert!(preserved.starts_with("config.corrupt."), "{preserved}");
    assert_eq!(read_file(&directory, &preserved), corrupt);
    #[cfg(unix)]
    assert_eq!(mode(&directory.join(&preserved)), 0o600);

    drop(store);
    let (store, startup) = open(scratch.root());
    assert!(matches!(
        startup.kind,
        StartupKind::Normal {
            backup: BackupState::Available { revision: 4 }
        }
    ));
    assert_eq!(store.config().revision(), 4);
}

#[test]
fn an_absent_main_is_recovered_without_a_preserved_copy() {
    let scratch = Scratch::new("store-recover-absent");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_BACKUP_FILE, &valid(4));

    let (store, startup) = open(root);
    match startup.kind {
        StartupKind::Recovered {
            backup,
            main_failure,
            preserved,
            commit,
        } => {
            assert_eq!(backup.revision(), 4);
            assert_eq!(main_failure, GenerationFailure::Absent);
            assert!(preserved.is_none());
            assert_eq!(commit.revision(), 4);
        }
        other => panic!("esito inatteso: {other:?}"),
    }

    assert_eq!(store.config().revision(), 4);
    assert_eq!(read_file(&directory, CONFIG_FILE), valid(4));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), valid(4));
    let names: Vec<String> = snapshot(&directory)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        vec![
            CONFIG_FILE.to_owned(),
            CONFIG_BACKUP_FILE.to_owned(),
            platform::LOCK_FILE.to_owned(),
        ],
        "nessuna copia del principale assente; resta il file di lock"
    );
}

#[test]
fn refusals_leave_the_root_unchanged() {
    // Schema futuro: nessuna reinterpretazione, nessuna scrittura, e i
    // temporanei del namespace riservato non vengono cancellati da un rifiuto.
    let scratch = Scratch::new("refuse-too-new");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, br#"{"schemaVersion":2,"revision":5}"#);
    write(&root, ".config.9.0.tmp", &valid(6));
    let before = state_snapshot(&directory);
    let locked = platform::lock(root).expect("lock di istanza acquisito");
    match Store::open(locked, COHERENT) {
        Err(ConfigError::TooNew {
            schema_version: 2, ..
        }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(state_snapshot(&directory), before);

    // Principale con schema precedente e backup con schema successivo: la
    // migrazione scarterebbe una generazione più nuova e viene rifiutata.
    let scratch = Scratch::new("refuse-schema-regression");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, br#"{"schemaVersion":0}"#);
    write(&root, CONFIG_BACKUP_FILE, &valid(4));
    let before = state_snapshot(&directory);
    let locked = platform::lock(root).expect("lock di istanza acquisita");
    match Store::open(locked, COHERENT) {
        Err(ConfigError::SchemaRegression { main: 0, backup: 1 }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(state_snapshot(&directory), before);

    // Schema precedente riconosciuto dall'intestazione ma non migrabile: è
    // corrotto e senza backup non esiste una generazione valida.
    let scratch = Scratch::new("refuse-unmigratable");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, br#"{"schemaVersion":0,"revision":0}"#);
    let before = state_snapshot(&directory);
    let locked = platform::lock(root).expect("lock di istanza acquisito");
    match Store::open(locked, COHERENT) {
        Err(ConfigError::NoValidGeneration { .. }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(state_snapshot(&directory), before);

    // Revisione in diminuzione rispetto al backup.
    let scratch = Scratch::new("refuse-regression");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, &valid(4));
    write(&root, CONFIG_BACKUP_FILE, &valid(5));
    let before = state_snapshot(&directory);
    let locked = platform::lock(root).expect("lock di istanza acquisito");
    match Store::open(locked, COHERENT) {
        Err(ConfigError::RevisionRegression { main: 4, backup: 5 }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(state_snapshot(&directory), before);

    // Componente di stato non ammesso: nessuna prima esecuzione.
    let scratch = Scratch::new("refuse-component");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let before = state_snapshot(&directory);
    let locked = platform::lock(root).expect("lock di istanza acquisito");
    match Store::open(
        locked,
        CoherenceInput {
            identity: ComponentState::NonInitial,
            registry: ComponentState::Initial,
        },
    ) {
        Err(ConfigError::ComponentState { .. }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(state_snapshot(&directory), before);

    // Configurazione presente senza identità.
    let scratch = Scratch::new("refuse-incoherent");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, &valid(1));
    let before = state_snapshot(&directory);
    let locked = platform::lock(root).expect("lock di istanza acquisito");
    match Store::open(
        locked,
        CoherenceInput {
            identity: ComponentState::Absent,
            registry: ComponentState::Initial,
        },
    ) {
        Err(ConfigError::IncoherentState { component }) => {
            assert_eq!(component, Component::Identity)
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(state_snapshot(&directory), before);

    // Protezioni errate: nessuna riparazione.
    #[cfg(unix)]
    {
        let scratch = Scratch::new("refuse-mode");
        let root = scratch.root();
        let directory = root.path().to_path_buf();
        write(&root, CONFIG_FILE, &valid(1));
        let path = directory.join(CONFIG_FILE);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("modalità impostata");
        let before = state_snapshot(&directory);
        let locked = platform::lock(root).expect("lock di istanza acquisito");
        match Store::open(locked, COHERENT) {
            Err(ConfigError::Protection(PlatformError::WrongMode { .. })) => {}
            other => panic!("esito inatteso: {other:?}"),
        }
        assert_eq!(state_snapshot(&directory), before);
        assert_eq!(mode(&path), 0o644, "la modalità non viene riparata");
    }
}

#[test]
fn revision_overflow_is_refused_without_writing() {
    let scratch = Scratch::new("store-overflow");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, &valid(u64::MAX));
    let before = state_snapshot(&directory);

    let (store, startup) = open(root);
    assert!(matches!(startup.kind, StartupKind::Normal { .. }));
    assert_eq!(store.config().revision(), u64::MAX);

    for _ in 0..2 {
        match store.save() {
            Err(ConfigError::RevisionOverflow { revision }) => assert_eq!(revision, u64::MAX),
            other => panic!("esito inatteso: {other:?}"),
        }
        assert_eq!(store.config().revision(), u64::MAX);
        assert_eq!(state_snapshot(&directory), before, "nessuna scrittura");
    }
}

#[test]
fn stale_temporaries_are_discarded_but_other_files_are_kept() {
    let scratch = Scratch::new("store-stale");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let process = std::process::id();
    let own_a = format!(".config.{process}.0.tmp");
    let own_b = format!(".config.{process}.1.tmp");
    write(&root, &own_a, &valid(7));
    write(&root, &own_b, &valid(8));
    write(&root, "config.json.old", b"vecchio");
    write(&root, "nota.txt", b"nota");
    write(&root, "config.corrupt.1.0", b"diagnostica");

    let (store, startup) = open(root);
    let mut discarded: Vec<String> = startup
        .discarded_temporaries
        .iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    discarded.sort();
    assert_eq!(discarded, vec![own_a.clone(), own_b.clone()]);
    assert!(!directory.join(&own_a).exists());
    assert!(!directory.join(&own_b).exists());
    assert_eq!(read_file(&directory, "config.json.old"), b"vecchio");
    assert_eq!(read_file(&directory, "nota.txt"), b"nota");
    assert_eq!(read_file(&directory, "config.corrupt.1.0"), b"diagnostica");
    assert_eq!(
        store.config().revision(),
        0,
        "i temporanei non vengono mai applicati"
    );
    assert_eq!(read_file(&directory, CONFIG_FILE), line(0));
    assert!(!directory.join(CONFIG_BACKUP_FILE).exists());
}

#[cfg(unix)]
#[test]
fn a_write_error_before_the_commit_preserves_the_previous_generation() {
    let scratch = Scratch::new("store-write-error");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let (store, _) = open(root);

    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500))
        .expect("permessi della radice ridotti");
    let error = store.save().expect_err("la scrittura non può riuscire");
    assert!(matches!(error, ConfigError::Write { .. }), "{error}");
    assert_eq!(read_file(&directory, CONFIG_FILE), line(0));
    assert!(!directory.join(CONFIG_BACKUP_FILE).exists());
    assert_eq!(store.config().revision(), 0);

    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .expect("permessi della radice ripristinati");
    let commit = store.save().expect("salvataggio successivo riuscito");
    assert_eq!(commit.revision(), 1);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(1));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), line(0));
}

// ---------------------------------------------------------------------------
// Rivalidazione prima del salvataggio
// ---------------------------------------------------------------------------

#[test]
fn a_main_replaced_with_a_future_schema_refuses_the_save() {
    let scratch = Scratch::new("save-too-new-main");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, "notes.txt", b"nota");
    let (store, _) = open(root);
    let main = directory.join(CONFIG_FILE);
    fs::write(&main, br#"{"schemaVersion":2,"revision":1}"#).expect("principale sostituito");
    let before = snapshot(&directory);

    match store.save() {
        Err(ConfigError::TooNew {
            schema_version: 2, ..
        }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(
        store.config().revision(),
        0,
        "la revisione in memoria resta"
    );
    assert_eq!(snapshot(&directory), before, "nessuna scrittura");
    assert_eq!(read_file(&directory, "notes.txt"), b"nota");
}

#[test]
fn a_backup_replaced_with_a_future_schema_refuses_the_save() {
    let scratch = Scratch::new("save-too-new-backup");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, "notes.txt", b"nota");
    let (store, _) = open(root);
    store.save().expect("primo salvataggio");
    let backup = directory.join(CONFIG_BACKUP_FILE);
    fs::write(&backup, br#"{"schemaVersion":2,"revision":0}"#).expect("backup sostituito");
    let before = snapshot(&directory);

    match store.save() {
        Err(ConfigError::TooNew {
            schema_version: 2, ..
        }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(store.config().revision(), 1);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(1));
    assert_eq!(snapshot(&directory), before, "il backup futuro resta com'è");
    assert_eq!(read_file(&directory, "notes.txt"), b"nota");
}

#[cfg(unix)]
#[test]
fn a_backup_with_changed_protections_refuses_the_save() {
    let scratch = Scratch::new("save-backup-mode");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, "notes.txt", b"nota");
    let (store, _) = open(root);
    store.save().expect("primo salvataggio");
    let backup = directory.join(CONFIG_BACKUP_FILE);
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o644)).expect("modalità impostata");
    let before = snapshot(&directory);

    match store.save() {
        Err(ConfigError::Protection(PlatformError::WrongMode {
            expected: 0o600,
            actual: 0o644,
            ..
        })) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(store.config().revision(), 1);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(1));
    assert_eq!(snapshot(&directory), before);
    assert_eq!(mode(&backup), 0o644, "nessuna riparazione");
    assert_eq!(read_file(&directory, "notes.txt"), b"nota");
}

#[test]
fn a_truncated_main_requires_an_explicit_recovery_instead_of_a_save() {
    let scratch = Scratch::new("save-corrupt-main");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, "notes.txt", b"nota");
    let (store, _) = open(root);
    store.save().expect("primo salvataggio");
    let corrupt = b"{\"schemaVersion\":1,".to_vec();
    fs::write(directory.join(CONFIG_FILE), &corrupt).expect("principale troncato");
    let before = snapshot(&directory);

    match store.save() {
        Err(ConfigError::RecoveryRequired {
            main: GenerationFailure::Corrupt { .. },
        }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(store.config().revision(), 1);
    assert_eq!(read_file(&directory, CONFIG_FILE), corrupt);
    assert_eq!(
        read_file(&directory, CONFIG_BACKUP_FILE),
        line(0),
        "il backup valido non viene sovrascritto dai byte corrotti"
    );
    assert_eq!(snapshot(&directory), before);
    assert_eq!(read_file(&directory, "notes.txt"), b"nota");
}

#[test]
fn a_divergent_revision_is_refused_without_adoption() {
    let scratch = Scratch::new("save-divergence");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, "notes.txt", b"nota");
    let (store, _) = open(root);
    fs::write(directory.join(CONFIG_FILE), valid(5)).expect("principale divergente");
    let before = snapshot(&directory);

    match store.save() {
        Err(ConfigError::RevisionDivergence {
            expected: 0,
            found: 5,
        }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(
        store.config().revision(),
        0,
        "la revisione non viene adottata"
    );
    assert_eq!(
        snapshot(&directory),
        before,
        "la generazione non viene sovrascritta"
    );

    fs::write(directory.join(CONFIG_FILE), line(0)).expect("generazione attesa ripristinata");
    let commit = store.save().expect("salvataggio successivo");
    assert_eq!(commit.revision(), 1);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(1));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), line(0));
}

#[test]
fn a_first_run_with_absent_components_can_still_save() {
    let scratch = Scratch::new("save-first-run-absent");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let (store, startup) = open_with(
        root,
        CoherenceInput {
            identity: ComponentState::Absent,
            registry: ComponentState::Absent,
        },
    );
    assert!(matches!(startup.kind, StartupKind::FirstRun { .. }));

    let commit = store.save().expect("salvataggio riuscito");
    assert_eq!(commit.revision(), 1);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(1));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), line(0));
}

#[test]
fn a_vanished_main_is_not_recreated_by_a_save() {
    let scratch = Scratch::new("save-vanished-main");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, "notes.txt", b"nota");
    let (store, _) = open(root);
    fs::remove_file(directory.join(CONFIG_FILE)).expect("principale rimosso");
    let before = snapshot(&directory);

    match store.save() {
        Err(ConfigError::NoValidGeneration {
            main: GenerationFailure::Absent,
            backup: GenerationFailure::Absent,
        }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(store.config().revision(), 0);
    assert_eq!(snapshot(&directory), before);
    assert_eq!(read_file(&directory, "notes.txt"), b"nota");
}

#[cfg(unix)]
#[test]
fn changed_protections_after_opening_refuse_the_save() {
    let scratch = Scratch::new("store-protection");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let (store, _) = open(root);

    let path = directory.join(CONFIG_FILE);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("modalità impostata");
    let error = store
        .save()
        .expect_err("la protezione alterata va rifiutata");
    assert!(
        matches!(
            error,
            ConfigError::Protection(PlatformError::WrongMode {
                expected: 0o600,
                actual: 0o644,
                ..
            })
        ),
        "errore inatteso: {error}"
    );
    assert_eq!(mode(&path), 0o644, "nessuna riparazione");
    assert_eq!(store.config().revision(), 0);
    assert!(!directory.join(CONFIG_BACKUP_FILE).exists());

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("modalità ripristinata");
    let commit = store.save().expect("salvataggio successivo riuscito");
    assert_eq!(commit.revision(), 1);
}

// ---------------------------------------------------------------------------
// Migrazioni in avanti (D3 P6)
// ---------------------------------------------------------------------------

/// Fixture sintetica dello schema 0: il solo `schemaVersion`, senza a capo.
/// Non è un formato storico reale; serve a provare il meccanismo `v0 → v1`.
const V0: &[u8] = br#"{"schemaVersion":0}"#;

#[test]
fn a_v0_main_is_migrated_keeping_the_original_bytes_in_the_backup() {
    let scratch = Scratch::new("migrate-main");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, V0);

    let (store, startup) = open(root);
    match startup.kind {
        StartupKind::Migrated {
            source,
            from_schema_version,
            previous_revision,
            preserved,
            commit,
        } => {
            assert_eq!(source, MigrationSource::Main);
            assert_eq!(from_schema_version, 0);
            assert_eq!(previous_revision, 0);
            assert!(
                preserved.is_none(),
                "sorgente principale: nulla da conservare"
            );
            assert_eq!(commit.revision(), 1);
            assert_durable(&commit);
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert!(startup.discarded_temporaries.is_empty());
    assert_eq!(store.config().revision(), 1);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(1));
    assert_eq!(
        read_file(&directory, CONFIG_BACKUP_FILE),
        V0,
        "il backup conserva i byte originali della generazione migrata"
    );
    #[cfg(unix)]
    assert_eq!(mode(&directory.join(CONFIG_BACKUP_FILE)), 0o600);

    // La riapertura trova una v1 con backup precedente: nessuna seconda
    // migrazione e nessuna scrittura.
    let before = snapshot(&directory);
    drop(store);
    let (store, startup) = open(scratch.root());
    assert!(matches!(
        startup.kind,
        StartupKind::Normal {
            backup: BackupState::OlderSchema { schema_version: 0 }
        }
    ));
    assert!(startup.discarded_temporaries.is_empty());
    assert_eq!(store.config().revision(), 1);
    assert_eq!(snapshot(&directory), before);

    // Il salvataggio successivo parte dalla revisione migrata.
    let commit = store.save().expect("salvataggio dopo la migrazione");
    assert_eq!(commit.revision(), 2);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(2));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), line(1));
}

#[test]
fn a_v0_backup_migrates_when_the_main_is_absent() {
    let scratch = Scratch::new("migrate-absent");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_BACKUP_FILE, V0);

    let (store, startup) = open(root);
    match startup.kind {
        StartupKind::Migrated {
            source,
            from_schema_version,
            previous_revision,
            preserved,
            commit,
        } => {
            assert_eq!(source, MigrationSource::Backup);
            assert_eq!(from_schema_version, 0);
            assert_eq!(previous_revision, 0);
            assert!(preserved.is_none(), "nessun principale da conservare");
            assert_eq!(commit.revision(), 1);
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(store.config().revision(), 1);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(1));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), V0);
}

#[test]
fn a_corrupt_main_with_a_v0_backup_is_preserved_and_the_backup_migrated() {
    let scratch = Scratch::new("migrate-corrupt");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let corrupt = b"{\"schemaVersion\":1,".to_vec();
    write(&root, CONFIG_FILE, &corrupt);
    write(&root, CONFIG_BACKUP_FILE, V0);

    let (store, startup) = open(root);
    let preserved = match startup.kind {
        StartupKind::Migrated {
            source,
            from_schema_version,
            preserved,
            commit,
            ..
        } => {
            assert_eq!(source, MigrationSource::Backup);
            assert_eq!(from_schema_version, 0);
            assert_eq!(commit.revision(), 1);
            preserved.expect("copia del principale corrotto")
        }
        other => panic!("esito inatteso: {other:?}"),
    };

    assert_eq!(store.config().revision(), 1);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(1));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), V0);
    let preserved = preserved.to_string_lossy().into_owned();
    assert!(preserved.starts_with("config.corrupt."), "{preserved}");
    assert_eq!(read_file(&directory, &preserved), corrupt);
}

#[test]
fn unmigratable_v0_documents_are_corrupt_and_never_migrated() {
    for (label, contents) in [
        (
            "v0-with-revision",
            &br#"{"schemaVersion":0,"revision":0}"#[..],
        ),
        (
            "v0-unknown-field",
            &br#"{"schemaVersion":0,"enabled":true}"#[..],
        ),
        (
            "v0-duplicate",
            &br#"{"schemaVersion":0,"schemaVersion":0}"#[..],
        ),
        ("v0-wrong-type", &br#"{"schemaVersion":"0"}"#[..]),
        ("v0-truncated", &br#"{"schemaVersion":0"#[..]),
    ] {
        let scratch = Scratch::new(label);
        let root = scratch.root();
        write(&root, CONFIG_FILE, contents);
        let before = snapshot(root.path());
        assert!(
            matches!(
                refused(classify(&root)),
                ConfigError::NoValidGeneration { .. }
            ),
            "{label}"
        );
        assert_eq!(snapshot(root.path()), before, "{label}");
    }

    // Con un backup v1 valido il principale non migrabile è corrotto: si
    // recupera, non si migra.
    let scratch = Scratch::new("v0-unmigratable-recover");
    let root = scratch.root();
    write(&root, CONFIG_FILE, br#"{"schemaVersion":0,"revision":0}"#);
    write(&root, CONFIG_BACKUP_FILE, &valid(4));
    match classify(&root) {
        Decision::RecoveryRequired { backup, main } => {
            assert_eq!(backup.revision(), 4);
            assert!(matches!(main, GenerationFailure::Corrupt { .. }));
        }
        other => panic!("decisione inattesa: {other:?}"),
    }
}

#[test]
fn a_v0_document_over_the_size_limit_is_refused() {
    let scratch = Scratch::new("v0-too-large");
    let root = scratch.root();
    let mut contents = V0.to_vec();
    contents.resize(64 * 1024 + 1, b' ');
    write(&root, CONFIG_FILE, &contents);
    let before = snapshot(root.path());

    assert!(matches!(
        refused(classify(&root)),
        ConfigError::TooLarge { limit: 65536, .. }
    ));
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn non_object_documents_are_corrupt_and_never_migrated() {
    // La sequenza `[0]` veniva accettata come schema sintetico 0 dalla
    // deserializzazione posizionale: la forma oggetto è ora obbligatoria.
    for (label, contents) in [
        ("array-zero", &br#"[0]"#[..]),
        ("array-zero-zero", &br#"[0,0]"#[..]),
        ("array-one-zero", &br#"[1,0]"#[..]),
        ("array-two", &br#"[2]"#[..]),
        ("array-empty", &br#"[]"#[..]),
        ("scalar-zero", &br#"0"#[..]),
        ("scalar-null", &br#"null"#[..]),
    ] {
        let scratch = Scratch::new(label);
        let root = scratch.root();
        let directory = root.path().to_path_buf();
        write(&root, CONFIG_FILE, contents);

        assert!(
            matches!(
                refused(classify(&root)),
                ConfigError::NoValidGeneration {
                    main: GenerationFailure::Corrupt { .. },
                    ..
                }
            ),
            "{label}"
        );

        let before = state_snapshot(&directory);
        let locked = platform::lock(root).expect("lock di istanza acquisito");
        match Store::open(locked, COHERENT) {
            Err(ConfigError::NoValidGeneration { .. }) => {}
            other => panic!("{label}: esito inatteso: {other:?}"),
        }
        assert_eq!(read_file(&directory, CONFIG_FILE), contents, "{label}");
        assert!(
            !directory.join(CONFIG_BACKUP_FILE).exists(),
            "{label}: nessun backup creato"
        );
        assert_eq!(state_snapshot(&directory), before, "{label}");
    }
}

#[test]
fn a_non_object_backup_is_corrupt_and_never_migrated_with_an_absent_main() {
    let scratch = Scratch::new("non-object-backup");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_BACKUP_FILE, b"[0]");

    assert!(matches!(
        refused(classify(&root)),
        ConfigError::NoValidGeneration {
            main: GenerationFailure::Absent,
            backup: GenerationFailure::Corrupt { .. },
        }
    ));

    let before = state_snapshot(&directory);
    let locked = platform::lock(root).expect("lock di istanza acquisito");
    match Store::open(locked, COHERENT) {
        Err(ConfigError::NoValidGeneration { .. }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert!(!directory.join(CONFIG_FILE).exists(), "nessuna migrazione");
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), b"[0]");
    assert_eq!(state_snapshot(&directory), before);
}

#[test]
fn a_non_object_main_recovers_from_a_valid_backup() {
    // La sequenza è rifiutata come sorgente di migrazione, ma resta un
    // principale corrotto: con un backup v1 valido si applica il recupero P4.
    let scratch = Scratch::new("non-object-recover");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, b"[0]");
    write(&root, CONFIG_BACKUP_FILE, &valid(4));

    match classify(&root) {
        Decision::RecoveryRequired { backup, main } => {
            assert_eq!(backup.revision(), 4);
            assert!(matches!(main, GenerationFailure::Corrupt { .. }));
        }
        other => panic!("decisione inattesa: {other:?}"),
    }

    let (store, startup) = open(root);
    let preserved = match startup.kind {
        StartupKind::Recovered {
            backup,
            main_failure,
            preserved,
            commit,
        } => {
            assert_eq!(backup.revision(), 4);
            assert!(matches!(main_failure, GenerationFailure::Corrupt { .. }));
            assert_eq!(commit.revision(), 4);
            preserved.expect("copia del principale corrotto")
        }
        other => panic!("esito inatteso: {other:?}"),
    };
    assert_eq!(store.config().revision(), 4);
    assert_eq!(read_file(&directory, CONFIG_FILE), valid(4));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), valid(4));
    let preserved = preserved.to_string_lossy().into_owned();
    assert_eq!(read_file(&directory, &preserved), b"[0]");
}

#[test]
fn a_non_object_backup_with_a_valid_main_is_signalled_corrupt_without_migration() {
    let scratch = Scratch::new("non-object-reserved");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    write(&root, CONFIG_FILE, &valid(2));
    write(&root, CONFIG_BACKUP_FILE, b"[0]");
    let before = state_snapshot(&directory);

    match classify(&root) {
        Decision::Normal { config, backup } => {
            assert_eq!(config.revision(), 2);
            assert!(matches!(backup, BackupState::Corrupt { .. }));
        }
        other => panic!("decisione inattesa: {other:?}"),
    }

    let (store, startup) = open(root);
    assert!(matches!(
        startup.kind,
        StartupKind::Normal {
            backup: BackupState::Corrupt { .. }
        }
    ));
    assert!(startup.discarded_temporaries.is_empty());
    assert_eq!(store.config().revision(), 2);
    assert_eq!(state_snapshot(&directory), before, "nessuna scrittura");
}

#[test]
fn a_formatted_v0_main_is_still_migrated_with_its_original_bytes() {
    let scratch = Scratch::new("migrate-formatted");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let original = b"{\n  \"schemaVersion\" : 0\n}\r\n";
    write(&root, CONFIG_FILE, original);

    let (store, startup) = open(root);
    match startup.kind {
        StartupKind::Migrated {
            source,
            from_schema_version,
            commit,
            ..
        } => {
            assert_eq!(source, MigrationSource::Main);
            assert_eq!(from_schema_version, 0);
            assert_eq!(commit.revision(), 1);
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(store.config().revision(), 1);
    assert_eq!(read_file(&directory, CONFIG_FILE), line(1));
    assert_eq!(read_file(&directory, CONFIG_BACKUP_FILE), original);
}

#[test]
fn a_main_replaced_with_a_v0_schema_refuses_the_save() {
    let scratch = Scratch::new("save-v0-main");
    let root = scratch.root();
    let directory = root.path().to_path_buf();
    let (store, _) = open(root);
    fs::write(directory.join(CONFIG_FILE), V0).expect("principale sostituito");
    let before = snapshot(&directory);

    match store.save() {
        Err(ConfigError::MigrationRequired {
            from_schema_version: 0,
            source: MigrationSource::Main,
        }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(store.config().revision(), 0);
    assert_eq!(snapshot(&directory), before, "nessuna scrittura");
}
