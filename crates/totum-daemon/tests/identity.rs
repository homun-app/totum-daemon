//! Prove d'integrazione dell'identità iniziale (D3 P7).
//!
//! Ogni prova usa una radice temporanea creata dalle primitive verificate di
//! `platform`; nessun profilo o configurazione reale viene toccata. Gli input
//! sono sintetici. Le prove specifiche di tipo POSIX sono marcate `cfg(unix)`;
//! i reparse point di Windows sono verificati soltanto in compilazione dal test
//! `platform_windows`.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use totum_daemon::config::{
    CONFIG_BACKUP_FILE, CONFIG_FILE, CoherenceInput, Component, ComponentState, ConfigError,
    Decision, StartupKind, Store,
};
use totum_daemon::identity::{
    self, CONTROLLERS_FILE, IDENTITY_FILE, IdentityError, MAX_IDENTITY_BYTES, Observation,
};
use totum_daemon::platform::{self, LOCK_FILE, LockedRoot, PlatformError, VerifiedRoot};

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
            .join("target/identity-integration-tests");
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

    fn directory(&self) -> PathBuf {
        self.path.join("totum")
    }

    fn root(&self) -> VerifiedRoot {
        platform::prepare_root(&self.directory()).expect("radice preparabile")
    }

    fn locked(&self) -> LockedRoot {
        platform::lock(self.root()).expect("lock di istanza acquisito")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = fs::set_permissions(self.directory(), fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.path);
    }
}

const IDENTITY: &[u8] = b"{\"schemaVersion\":1,\"state\":\"notInitialized\"}\n";
const CONTROLLERS: &[u8] = b"{\"schemaVersion\":1,\"grantRevision\":0,\"controllers\":[]}\n";
const CONFIG: &[u8] = b"{\"schemaVersion\":1,\"revision\":3}\n";

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

fn initial_entries() -> Vec<String> {
    let mut names = vec![
        CONTROLLERS_FILE.to_owned(),
        IDENTITY_FILE.to_owned(),
        LOCK_FILE.to_owned(),
    ];
    names.sort();
    names
}

fn created_names(startup: &identity::Startup) -> Vec<&'static str> {
    startup
        .created
        .iter()
        .map(identity::Created::file_name)
        .collect()
}

fn classify(root: &VerifiedRoot) -> identity::Classification {
    identity::classify(root).expect("classificazione riuscita")
}

fn expect_incoherent(error: identity::InitError, component: Component) {
    match error.error {
        IdentityError::IncoherentState { component: actual } => assert_eq!(actual, component),
        other => panic!("errore inatteso: {other}"),
    }
}

fn expect_config_refused(error: identity::InitError) -> ConfigError {
    match error.error {
        IdentityError::ConfigRefused(inner) => *inner,
        other => panic!("errore inatteso: {other}"),
    }
}

#[cfg(unix)]
fn inode(path: &Path) -> u64 {
    fs::symlink_metadata(path)
        .expect("metadati disponibili")
        .ino()
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
fn first_initialization_creates_both_documents_and_the_reopen_changes_nothing() {
    let scratch = Scratch::new("first");
    let directory = scratch.directory();

    let startup = {
        let mut locked = scratch.locked();
        identity::initialize(&mut locked).expect("prima inizializzazione")
    };
    assert_eq!(
        created_names(&startup),
        vec![IDENTITY_FILE, CONTROLLERS_FILE],
        "l'identità è pubblicata prima del registro"
    );
    assert!(startup.identity.is_initial());
    assert_eq!(startup.identity.grant_revision(), 0);
    assert_eq!(startup.identity.controller_count(), 0);
    assert_eq!(startup.discarded_temporaries, Vec::<OsString>::new());
    assert!(
        startup
            .created
            .iter()
            .all(|created| created.commit().durable())
    );
    assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
    assert_eq!(read(&directory, CONTROLLERS_FILE), CONTROLLERS);
    assert_eq!(entries(&directory), initial_entries());

    #[cfg(unix)]
    {
        assert_eq!(mode(&directory.join(IDENTITY_FILE)), 0o600);
        assert_eq!(mode(&directory.join(CONTROLLERS_FILE)), 0o600);
    }

    #[cfg(unix)]
    let identity_inode = inode(&directory.join(IDENTITY_FILE));
    #[cfg(unix)]
    let controllers_inode = inode(&directory.join(CONTROLLERS_FILE));

    let startup = {
        let mut locked = scratch.locked();
        identity::initialize(&mut locked).expect("riapertura")
    };
    assert!(startup.created.is_empty(), "nessuna riscrittura");
    assert!(startup.discarded_temporaries.is_empty());

    #[cfg(unix)]
    {
        assert_eq!(inode(&directory.join(IDENTITY_FILE)), identity_inode);
        assert_eq!(inode(&directory.join(CONTROLLERS_FILE)), controllers_inode);
    }
    assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
    assert_eq!(read(&directory, CONTROLLERS_FILE), CONTROLLERS);

    let root = scratch.root();
    let classification = classify(&root);
    assert!(classification.identity.is_initial());
    assert!(classification.controllers.is_initial());
    assert_eq!(
        classification.coherence(),
        CoherenceInput {
            identity: ComponentState::Initial,
            registry: ComponentState::Initial,
        }
    );
}

#[test]
fn an_existing_initial_document_is_kept_and_only_the_missing_one_is_created() {
    for (present, expected_created) in [
        (IDENTITY_FILE, vec![CONTROLLERS_FILE]),
        (CONTROLLERS_FILE, vec![IDENTITY_FILE]),
    ] {
        let scratch = Scratch::new("partial");
        let directory = scratch.directory();
        let root = scratch.root();
        let contents = if present == IDENTITY_FILE {
            IDENTITY
        } else {
            CONTROLLERS
        };
        write(&root, present, contents);
        drop(root);

        #[cfg(unix)]
        let before = inode(&directory.join(present));

        let startup = {
            let mut locked = scratch.locked();
            identity::initialize(&mut locked).expect("completamento")
        };
        assert_eq!(created_names(&startup), expected_created, "{present}");
        assert_eq!(read(&directory, present), contents);

        #[cfg(unix)]
        assert_eq!(
            inode(&directory.join(present)),
            before,
            "{present}: il file esistente non viene riscritto"
        );
        assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
        assert_eq!(read(&directory, CONTROLLERS_FILE), CONTROLLERS);
        assert_eq!(entries(&directory), initial_entries());
    }
}

#[test]
fn classification_keeps_absence_distinct_from_an_initial_document() {
    let scratch = Scratch::new("classify");
    let root = scratch.root();
    let directory = scratch.directory();

    let classification = classify(&root);
    assert!(classification.identity.is_absent());
    assert!(classification.controllers.is_absent());
    assert_eq!(
        classification.coherence(),
        CoherenceInput {
            identity: ComponentState::Absent,
            registry: ComponentState::Absent,
        }
    );
    assert_eq!(entries(&directory), Vec::<String>::new(), "sola lettura");

    write(&root, IDENTITY_FILE, IDENTITY);
    let classification = classify(&root);
    assert!(classification.identity.is_initial());
    assert!(classification.controllers.is_absent());
    assert_eq!(
        classification.coherence(),
        CoherenceInput {
            identity: ComponentState::Initial,
            registry: ComponentState::Absent,
        }
    );
    assert_eq!(entries(&directory), vec![IDENTITY_FILE.to_owned()]);
}

#[test]
fn a_configuration_with_a_missing_component_is_incoherent_and_blocks_creation() {
    let cases: [(&str, &[u8], &[u8], Component); 2] = [
        ("main", CONFIG, b"", Component::Identity),
        ("backup", b"", CONFIG, Component::Identity),
    ];
    for (label, main, backup, missing) in cases {
        let scratch = Scratch::new("incoherent");
        let root = scratch.root();
        let directory = scratch.directory();
        if !main.is_empty() {
            write(&root, CONFIG_FILE, main);
        }
        if !backup.is_empty() {
            write(&root, CONFIG_BACKUP_FILE, backup);
        }
        drop(root);

        let error = {
            let mut locked = scratch.locked();
            identity::initialize(&mut locked).expect_err(label)
        };
        expect_incoherent(error, missing);
        assert!(!directory.join(IDENTITY_FILE).exists(), "{label}");
        assert!(!directory.join(CONTROLLERS_FILE).exists(), "{label}");
        if !main.is_empty() {
            assert_eq!(read(&directory, CONFIG_FILE), main);
        }
        if !backup.is_empty() {
            assert_eq!(read(&directory, CONFIG_BACKUP_FILE), backup);
        }
    }

    let scratch = Scratch::new("incoherent-registry");
    let root = scratch.root();
    let directory = scratch.directory();
    write(&root, CONFIG_FILE, CONFIG);
    write(&root, IDENTITY_FILE, IDENTITY);
    drop(root);
    let error = {
        let mut locked = scratch.locked();
        identity::initialize(&mut locked).expect_err("registro mancante")
    };
    expect_incoherent(error, Component::Registry);
    assert!(!directory.join(CONTROLLERS_FILE).exists());
    assert_eq!(read(&directory, IDENTITY_FILE), IDENTITY);
}

#[test]
fn a_configuration_with_initial_components_needs_no_writes() {
    let scratch = Scratch::new("coherent");
    let root = scratch.root();
    let directory = scratch.directory();
    write(&root, CONFIG_FILE, CONFIG);
    write(&root, CONFIG_BACKUP_FILE, CONFIG);
    write(&root, IDENTITY_FILE, IDENTITY);
    write(&root, CONTROLLERS_FILE, CONTROLLERS);
    drop(root);

    let startup = {
        let mut locked = scratch.locked();
        identity::initialize(&mut locked).expect("stato coerente")
    };
    assert!(startup.created.is_empty());
    assert_eq!(entries(&directory), {
        let mut names = initial_entries();
        names.push(CONFIG_FILE.to_owned());
        names.push(CONFIG_BACKUP_FILE.to_owned());
        names.sort();
        names
    });
}

#[test]
fn non_initial_documents_are_refused_and_never_corrected() {
    for (file, contents) in [
        (
            IDENTITY_FILE,
            br#"{"schemaVersion":1,"state":"initialized"}"#.as_slice(),
        ),
        (
            CONTROLLERS_FILE,
            br#"{"schemaVersion":1,"grantRevision":1,"controllers":[]}"#.as_slice(),
        ),
        (
            CONTROLLERS_FILE,
            br#"{"schemaVersion":1,"grantRevision":0,"controllers":[{}]}"#.as_slice(),
        ),
    ] {
        let scratch = Scratch::new("non-initial");
        let root = scratch.root();
        write(&root, file, contents);
        drop(root);

        let error = {
            let mut locked = scratch.locked();
            identity::initialize(&mut locked).expect_err("non iniziale")
        };
        match error.error {
            IdentityError::NonInitial { component, .. } => {
                let expected = if file == IDENTITY_FILE {
                    Component::Identity
                } else {
                    Component::Registry
                };
                assert_eq!(component, expected);
            }
            other => panic!("errore inatteso: {other}"),
        }
        assert_eq!(
            read(&scratch.directory(), file),
            contents,
            "nessuna correzione"
        );
        let other = if file == IDENTITY_FILE {
            CONTROLLERS_FILE
        } else {
            IDENTITY_FILE
        };
        assert!(
            !scratch.directory().join(other).exists(),
            "nessuna ricreazione"
        );
    }
}

#[test]
fn malformed_documents_are_unusable_and_not_absent() {
    let oversized = vec![b'x'; usize::try_from(MAX_IDENTITY_BYTES).expect("limite") + 1];
    let identity_cases: Vec<(&str, &[u8])> = vec![
        ("non-object", b"[1]"),
        (
            "duplicate",
            br#"{"schemaVersion":1,"state":"notInitialized","state":"notInitialized"}"#,
        ),
        ("wrong-type", br#"{"schemaVersion":1,"state":1}"#),
        (
            "truncated",
            br#"{"schemaVersion":1,"state":"notInitialized""#,
        ),
        (
            "unknown",
            br#"{"schemaVersion":1,"state":"notInitialized","extra":true}"#,
        ),
        ("future", br#"{"schemaVersion":2,"state":"notInitialized"}"#),
        ("oversized", oversized.as_slice()),
    ];
    for (label, contents) in identity_cases {
        let scratch = Scratch::new("malformed-identity");
        let root = scratch.root();
        write(&root, IDENTITY_FILE, contents);
        drop(root);

        let classification = classify(&scratch.root());
        assert!(
            matches!(classification.identity, Observation::Unusable(_)),
            "{label}"
        );
        assert!(classification.controllers.is_absent(), "{label}");

        let error = {
            let mut locked = scratch.locked();
            identity::initialize(&mut locked).expect_err(label)
        };
        let expected = match label {
            "future" => matches!(
                error.error,
                IdentityError::UnsupportedSchema {
                    schema_version: 2,
                    ..
                }
            ),
            "oversized" => matches!(
                error.error,
                IdentityError::TooLarge { limit, .. } if limit == MAX_IDENTITY_BYTES
            ),
            _ => matches!(error.error, IdentityError::InvalidDocument { .. }),
        };
        assert!(expected, "{label}: {error}");
        assert!(!scratch.directory().join(CONTROLLERS_FILE).exists());
        assert_eq!(read(&scratch.directory(), IDENTITY_FILE), contents);
    }

    let controllers_cases: Vec<(&str, &[u8])> = vec![
        ("non-object", b"[]"),
        (
            "duplicate",
            br#"{"schemaVersion":1,"grantRevision":0,"grantRevision":0,"controllers":[]}"#,
        ),
        (
            "wrong-type",
            br#"{"schemaVersion":1,"grantRevision":0,"controllers":{}}"#,
        ),
        (
            "truncated",
            br#"{"schemaVersion":1,"grantRevision":0,"controllers":["#,
        ),
        (
            "future",
            br#"{"schemaVersion":2,"grantRevision":0,"controllers":[]}"#,
        ),
    ];
    for (label, contents) in controllers_cases {
        let scratch = Scratch::new("malformed-controllers");
        let root = scratch.root();
        write(&root, CONTROLLERS_FILE, contents);
        drop(root);

        let classification = classify(&scratch.root());
        assert!(
            matches!(classification.controllers, Observation::Unusable(_)),
            "{label}"
        );
        assert!(classification.identity.is_absent(), "{label}");

        let error = {
            let mut locked = scratch.locked();
            identity::initialize(&mut locked).expect_err(label)
        };
        if label == "future" {
            assert!(matches!(
                error.error,
                IdentityError::UnsupportedSchema {
                    schema_version: 2,
                    ..
                }
            ));
        } else {
            assert!(matches!(error.error, IdentityError::InvalidDocument { .. }));
        }
        assert!(!scratch.directory().join(IDENTITY_FILE).exists());
    }
}

#[test]
fn a_future_schema_is_neither_absent_nor_migrated() {
    let scratch = Scratch::new("future");
    let root = scratch.root();
    write(
        &root,
        IDENTITY_FILE,
        br#"{"schemaVersion":9,"state":"notInitialized"}"#,
    );
    drop(root);

    let classification = classify(&scratch.root());
    assert!(matches!(
        classification.identity,
        Observation::Unusable(IdentityError::UnsupportedSchema {
            schema_version: 9,
            ..
        })
    ));
    assert_eq!(
        classification.identity_state(),
        ComponentState::Unusable,
        "uno schema futuro non è assenza"
    );

    let error = {
        let mut locked = scratch.locked();
        identity::initialize(&mut locked).expect_err("nessuna migrazione")
    };
    assert!(matches!(
        error.error,
        IdentityError::UnsupportedSchema {
            schema_version: 9,
            ..
        }
    ));
    assert!(!scratch.directory().join(CONTROLLERS_FILE).exists());
    assert_eq!(
        entries(&scratch.directory()),
        vec![IDENTITY_FILE.to_owned(), LOCK_FILE.to_owned()],
        "nessun backup né documento trasformato"
    );
}

#[test]
fn stale_identity_temporaries_are_discarded_while_foreign_names_are_kept() {
    let scratch = Scratch::new("temporaries");
    let root = scratch.root();
    let directory = scratch.directory();
    write(&root, ".identity.4242.0.tmp", IDENTITY);
    write(&root, ".config.4242.0.tmp", CONFIG);
    write(&root, "notes.txt", b"nota");
    drop(root);

    let startup = {
        let mut locked = scratch.locked();
        identity::initialize(&mut locked).expect("prima inizializzazione")
    };
    assert_eq!(startup.discarded_temporaries.len(), 1);
    assert!(!directory.join(".identity.4242.0.tmp").exists());
    assert_eq!(read(&directory, ".config.4242.0.tmp"), CONFIG);
    assert_eq!(read(&directory, "notes.txt"), b"nota");
}

#[test]
fn the_second_instance_is_refused_before_reading_or_writing() {
    let scratch = Scratch::new("second");
    let directory = scratch.directory();
    let mut locked = scratch.locked();

    match platform::lock(scratch.root()) {
        Err(PlatformError::AlreadyRunning { path }) => {
            assert_eq!(path, directory.join(LOCK_FILE));
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert!(!directory.join(IDENTITY_FILE).exists());
    assert!(!directory.join(CONTROLLERS_FILE).exists());

    let startup = identity::initialize(&mut locked).expect("prima istanza");
    assert_eq!(
        created_names(&startup),
        vec![IDENTITY_FILE, CONTROLLERS_FILE]
    );
}

#[test]
fn no_key_or_secret_material_is_written() {
    let scratch = Scratch::new("no-secrets");
    let directory = scratch.directory();
    {
        let mut locked = scratch.locked();
        identity::initialize(&mut locked).expect("prima inizializzazione");
    }

    assert_eq!(entries(&directory), initial_entries());
    for name in [IDENTITY_FILE, CONTROLLERS_FILE] {
        let text = String::from_utf8(read(&directory, name)).expect("UTF-8");
        let lower = text.to_ascii_lowercase();
        for forbidden in ["key", "token", "secret", "fingerprint", "private", "public"] {
            assert!(!lower.contains(forbidden), "{name}: {forbidden}");
        }
    }
}

#[test]
fn the_initial_documents_are_coherent_for_the_configuration_classification() {
    let scratch = Scratch::new("config-coherence");
    {
        let mut locked = scratch.locked();
        identity::initialize(&mut locked).expect("prima inizializzazione");
    }

    let root = scratch.root();
    let classification = classify(&root);
    let decision = totum_daemon::config::classify(&root, classification.coherence());
    assert!(matches!(decision, Decision::FirstRun { .. }));

    let (store, startup) = {
        let locked = scratch.locked();
        Store::open(locked, classification.coherence()).expect("apertura della configurazione")
    };
    assert!(matches!(startup.kind, StartupKind::FirstRun { .. }));
    assert_eq!(store.config().revision(), 0);
    drop(store);

    let startup = {
        let mut locked = scratch.locked();
        identity::initialize(&mut locked).expect("stato coerente con configurazione")
    };
    assert!(startup.created.is_empty());
}

#[test]
fn a_corrupt_configuration_blocks_creation() {
    // Con i componenti presenti la coerenza è soddisfatta: il rifiuto riguarda
    // la generazione della configurazione, non un componente mancante.
    let scratch = Scratch::new("corrupt-config");
    let root = scratch.root();
    write(&root, CONFIG_FILE, b"{\"schemaVersion\":1,");
    write(&root, IDENTITY_FILE, IDENTITY);
    write(&root, CONTROLLERS_FILE, CONTROLLERS);
    drop(root);

    let error = {
        let mut locked = scratch.locked();
        identity::initialize(&mut locked).expect_err("configurazione corrotta")
    };
    assert!(matches!(
        expect_config_refused(error),
        ConfigError::NoValidGeneration { .. }
    ));
    assert_eq!(
        read(&scratch.directory(), CONFIG_FILE),
        b"{\"schemaVersion\":1,"
    );
    assert_eq!(read(&scratch.directory(), IDENTITY_FILE), IDENTITY);
    assert_eq!(read(&scratch.directory(), CONTROLLERS_FILE), CONTROLLERS);

    // Con i componenti assenti la precedenza è della coerenza: una
    // configurazione presente senza identità o registro non viene interpretata
    // come prima esecuzione.
    let missing = Scratch::new("corrupt-config-missing");
    let root = missing.root();
    write(&root, CONFIG_FILE, b"{\"schemaVersion\":1,");
    drop(root);
    let error = {
        let mut locked = missing.locked();
        identity::initialize(&mut locked).expect_err("componenti mancanti")
    };
    expect_incoherent(error, Component::Identity);
    assert!(!missing.directory().join(IDENTITY_FILE).exists());

    let too_new = Scratch::new("future-config");
    let root = too_new.root();
    write(&root, CONFIG_FILE, b"{\"schemaVersion\":2,\"revision\":0}");
    drop(root);
    let error = {
        let mut locked = too_new.locked();
        identity::initialize(&mut locked).expect_err("configurazione futura")
    };
    assert!(matches!(
        expect_config_refused(error),
        ConfigError::TooNew {
            schema_version: 2,
            ..
        }
    ));
}

#[cfg(unix)]
#[test]
fn weakened_protections_and_symlinks_block_modifications() {
    let scratch = Scratch::new("weak-mode");
    let root = scratch.root();
    write(&root, IDENTITY_FILE, IDENTITY);
    drop(root);
    fs::set_permissions(
        scratch.directory().join(IDENTITY_FILE),
        fs::Permissions::from_mode(0o644),
    )
    .expect("modalità indebolita");

    let classification = classify(&scratch.root());
    assert!(matches!(
        classification.identity,
        Observation::Unusable(IdentityError::Protection(PlatformError::WrongMode { .. }))
    ));
    let error = {
        let mut locked = scratch.locked();
        identity::initialize(&mut locked).expect_err("protezioni errate")
    };
    assert!(matches!(
        error.error,
        IdentityError::Protection(PlatformError::WrongMode { .. })
    ));
    assert!(!scratch.directory().join(CONTROLLERS_FILE).exists());

    let linked = Scratch::new("symlink");
    let directory = linked.directory();
    let root = linked.root();
    write(&root, "target.json", IDENTITY);
    drop(root);
    symlink(directory.join("target.json"), directory.join(IDENTITY_FILE))
        .expect("collegamento creato");

    let classification = classify(&linked.root());
    assert!(matches!(
        classification.identity,
        Observation::Unusable(IdentityError::Protection(
            PlatformError::SymlinkRefused { .. }
        ))
    ));
    let error = {
        let mut locked = linked.locked();
        identity::initialize(&mut locked).expect_err("collegamento")
    };
    assert!(matches!(
        error.error,
        IdentityError::Protection(PlatformError::SymlinkRefused { .. })
    ));
    assert!(!directory.join(CONTROLLERS_FILE).exists());
    assert_eq!(read(&directory, "target.json"), IDENTITY, "mai seguito");
}
