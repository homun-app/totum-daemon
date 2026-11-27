//! Prove del lock di istanza unica (D3 P5) e della sua tenuta durante le
//! migrazioni (D3 P6).
//!
//! Ogni prova usa una radice temporanea creata dalle primitive verificate di
//! `platform`; nessun profilo o configurazione reale viene toccata. Le prove di
//! esclusione fra processi usano processi figli reali con sincronizzazione a
//! file di segnalazione e termini massimi; la pulizia resta al processo padre.
//! Le prove specifiche di tipo POSIX sono marcate `cfg(unix)`.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use totum_daemon::config::{
    CONFIG_BACKUP_FILE, CONFIG_FILE, CoherenceInput, ComponentState, Store,
};
use totum_daemon::platform::{self, LOCK_FILE, PlatformError, VerifiedRoot};

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
            .join("target/lock-tests");
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

fn valid(revision: u64) -> Vec<u8> {
    format!("{{\"schemaVersion\":1,\"revision\":{revision}}}").into_bytes()
}

fn write(root: &VerifiedRoot, name: &str, contents: &[u8]) {
    let mut file = root.create_file(OsStr::new(name)).expect("file creabile");
    file.write_all(contents).expect("contenuto scritto");
    file.sync_all().expect("contenuto sincronizzato");
}

fn read(directory: &Path, name: &str) -> Vec<u8> {
    fs::read(directory.join(name)).expect("file leggibile")
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("metadati disponibili")
        .permissions()
        .mode()
        & 0o7777
}

#[cfg(unix)]
fn inode(path: &Path) -> u64 {
    fs::symlink_metadata(path)
        .expect("metadati disponibili")
        .ino()
}

fn open_store(root: VerifiedRoot) -> (Store, totum_daemon::config::Startup) {
    let locked = platform::lock(root).expect("lock di istanza acquisito");
    Store::open(locked, COHERENT).expect("apertura riuscita")
}

/// Coordinazione dell'harness fra avvio dei processi figli e transizioni del
/// lock.
///
/// Un figlio forchettato eredita i descrittori aperti del padre finché non
/// esegue. `flock` è legato alla open file description: un riferimento
/// ereditato tiene il lock anche dopo che il padre ha chiuso il proprio
/// descrittore, finché il figlio non lo chiude a `exec`. Il prodotto non crea
/// processi; questa finestra esiste soltanto nelle prove, dove più test dello
/// stesso binario forkano processi figli. Chi rilascia e riacquisisce un lock
/// prende questo mutex, che [`spawn_child`] tiene per l'intera `spawn` (che
/// attende `exec`): nessun avvio di figlio può cadere fra il rilascio e la
/// riacquisizione. Non è un'attesa e non disattiva il parallelismo delle prove.
static FORKS: Mutex<()> = Mutex::new(());

fn fork_window() -> MutexGuard<'static, ()> {
    FORKS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn is_already_running(error: PlatformError, expected: &Path) {
    match error {
        PlatformError::AlreadyRunning { path } => {
            assert_eq!(path, expected, "il rifiuto nomina il file di lock")
        }
        other => panic!("esito inatteso: {other:?}"),
    }
}

#[test]
fn first_acquisition_creates_the_lock_and_release_keeps_it() {
    let scratch = Scratch::new("acquire-release");
    let path = scratch.path.join("totum").join(LOCK_FILE);
    assert!(!path.exists(), "il lock non esiste prima dell'acquisizione");

    let first = platform::lock(scratch.root()).expect("prima acquisizione");
    assert_eq!(first.lock_path(), path, "il guard nomina state.lock");
    assert!(path.exists(), "il file di lock viene creato");
    #[cfg(unix)]
    {
        assert_eq!(mode(&path), 0o600, "modalità del file di lock");
        assert_eq!(
            fs::symlink_metadata(&path).unwrap().uid(),
            rustix::process::geteuid().as_raw(),
            "il file di lock appartiene all'utente corrente"
        );
    }
    #[cfg(unix)]
    let before = inode(&path);

    let second = {
        let _forks = fork_window();
        drop(first);
        assert!(path.exists(), "il rilascio non cancella state.lock");
        #[cfg(unix)]
        assert_eq!(
            inode(&path),
            before,
            "il rilascio non sostituisce state.lock"
        );
        platform::lock(scratch.root()).expect("nuova acquisizione dopo il rilascio")
    };
    assert_eq!(second.lock_path(), path);
}

#[test]
fn a_second_acquisition_in_the_same_process_is_refused() {
    let scratch = Scratch::new("same-process");
    let path = scratch.path.join("totum").join(LOCK_FILE);

    let held = platform::lock(scratch.root()).expect("prima acquisizione");
    match platform::lock(scratch.root()) {
        Err(error) => is_already_running(error, &path),
        Ok(_) => panic!("la seconda acquisizione non deve riuscire"),
    }

    let _forks = fork_window();
    drop(held);
    let acquired = platform::lock(scratch.root()).expect("acquisizione dopo il rilascio");
    drop(acquired);
}

#[test]
fn the_lock_does_not_depend_on_the_file_contents() {
    let scratch = Scratch::new("contents");
    let directory = scratch.path.join("totum");
    let root = scratch.root();
    write(&root, LOCK_FILE, b"9999");

    let guard = platform::lock(root).expect("un contenuto arbitrario non occupa il lock");
    drop(guard);
    assert_eq!(
        read(&directory, LOCK_FILE),
        b"9999",
        "il contenuto non viene letto né riscritto"
    );
}

#[test]
fn a_second_store_is_refused_before_the_configuration_is_read() {
    let scratch = Scratch::new("store-refused");
    let directory = scratch.path.join("totum");
    let path = directory.join(LOCK_FILE);
    let root = scratch.root();
    // Schema futuro: se la classificazione precedesse il lock, l'esito sarebbe
    // `TooNew` invece di `alreadyRunning`.
    write(&root, CONFIG_FILE, br#"{"schemaVersion":2,"revision":5}"#);

    let held = platform::lock(root).expect("prima acquisizione");
    match platform::lock(scratch.root()) {
        Err(error) => is_already_running(error, &path),
        Ok(_) => panic!("la seconda istanza non deve superare il lock"),
    }
    assert_eq!(
        read(&directory, CONFIG_FILE),
        br#"{"schemaVersion":2,"revision":5}"#,
        "la configurazione non viene toccata prima del lock"
    );

    let _forks = fork_window();
    drop(held);
    let locked = platform::lock(scratch.root()).expect("acquisizione dopo il rilascio");
    match Store::open(locked, COHERENT) {
        Err(totum_daemon::config::ConfigError::TooNew {
            schema_version: 2, ..
        }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
}

#[test]
fn different_roots_have_independent_locks() {
    let first = Scratch::new("independent-a");
    let second = Scratch::new("independent-b");
    let first_root = first.root();
    let second_root = second.root();

    let (first_store, _) = open_store(first_root);
    let (second_store, _) = open_store(second_root);
    assert!(first.path.join("totum").join(LOCK_FILE).exists());
    assert!(second.path.join("totum").join(LOCK_FILE).exists());

    assert_eq!(first_store.save().expect("salvataggio").revision(), 1);
    assert_eq!(second_store.save().expect("salvataggio").revision(), 1);
    assert_eq!(first_store.config().revision(), 1);
    assert_eq!(second_store.config().revision(), 1);
}

#[test]
fn a_store_uses_only_the_root_it_locked() {
    let locked = Scratch::new("bound-locked");
    let unprotected = Scratch::new("bound-unprotected");
    let locked_directory = locked.path.join("totum");
    let unprotected_directory = unprotected.path.join("totum");

    let locked_root = locked.root();
    write(&locked_root, CONFIG_FILE, &valid(2));
    write(&locked_root, ".config.4242.0.tmp", &valid(9));
    let free_root = unprotected.root();
    write(&free_root, CONFIG_FILE, &valid(7));
    write(&free_root, CONFIG_BACKUP_FILE, &valid(6));
    write(&free_root, ".config.4242.0.tmp", &valid(8));
    write(&free_root, "notes.txt", b"nota");
    let untouched = snapshot(&unprotected_directory);

    // La radice e il lock sono un solo oggetto: lo Store può soltanto usare la
    // radice su cui il lock è stato acquisito. La radice libera non viene
    // classificata, ripulita né scritta, e non riceve un file di lock.
    let (store, startup) = open_store(locked_root);
    assert_eq!(store.config().revision(), 2);
    assert_eq!(
        startup.discarded_temporaries,
        vec![OsString::from(".config.4242.0.tmp")],
        "il temporaneo della radice bloccata viene scartato"
    );
    assert_eq!(store.save().expect("salvataggio").revision(), 3);

    let mut saved = valid(3);
    saved.push(b'\n');
    assert_eq!(read(&locked_directory, CONFIG_FILE), saved);
    assert_eq!(
        snapshot(&unprotected_directory),
        untouched,
        "la radice non protetta resta identica, compresi i suoi temporanei"
    );
    assert!(
        !unprotected_directory.join(LOCK_FILE).exists(),
        "nessuna acquisizione avviene sulla radice non protetta"
    );
}

#[test]
fn the_lock_follows_the_anchored_directory_not_the_nominal_path() {
    let scratch = Scratch::new("anchored");
    let directory = scratch.path.join("totum");
    let moved = scratch.path.join("moved");
    let held = platform::lock(scratch.root()).expect("prima acquisizione");

    // La directory ancorata viene rinominata e un'altra ne occupa il percorso
    // nominale: l'identità della directory, non il testo del percorso, decide
    // quale radice è protetta.
    let renamed = fs::rename(&directory, &moved);
    #[cfg(windows)]
    if let Err(error) = &renamed {
        // Windows puo' impedire la rinomina di una cartella contenente il
        // file bloccato. Provare il rifiuto e il lock conservato, poi ripetere
        // dopo il rilascio: non accettare un errore generico di permessi.
        assert!(matches!(error.raw_os_error(), Some(5 | 32)), "{error}");
        assert!(directory.is_dir());
        assert!(!moved.exists());
        match platform::lock(scratch.root()) {
            Err(error) => is_already_running(error, &directory.join(LOCK_FILE)),
            Ok(_) => panic!("la rinomina rifiutata non deve liberare il lock"),
        }
        drop(held);
        fs::rename(&directory, &moved).expect("rinomina riuscita dopo il rilascio");
        let moved_root = platform::prepare_root(&moved).expect("radice spostata valida");
        let acquired = platform::lock(moved_root).expect("lock riacquisibile dopo il rilascio");
        assert_eq!(acquired.lock_path(), moved.join(LOCK_FILE));
        eprintln!("Windows: rinomina rifiutata con lock vivo, riuscita dopo il rilascio");
        return;
    }
    renamed.expect("rinomina della radice");
    fs::create_dir(&directory).expect("nuova directory sullo stesso percorso");
    #[cfg(unix)]
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .expect("permessi della nuova directory");

    let replacement = platform::lock(platform::prepare_root(&directory).expect("nuova radice"))
        .expect("una directory diversa con lo stesso percorso non è protetta dal guard");
    drop(replacement);

    match platform::lock(platform::prepare_root(&moved).expect("radice spostata")) {
        Err(error) => is_already_running(error, &moved.join(LOCK_FILE)),
        Ok(_) => panic!("la stessa directory ancorata non può avere due lock"),
    }
    drop(held);
}

#[cfg(unix)]
#[test]
fn a_lock_of_the_wrong_type_is_refused() {
    let scratch = Scratch::new("wrong-type");
    let directory = scratch.path.join("totum");
    let root = scratch.root();
    fs::create_dir(directory.join(LOCK_FILE)).expect("directory creata");

    match platform::lock(root) {
        Err(PlatformError::NotARegularFile { path }) => {
            assert_eq!(path, directory.join(LOCK_FILE))
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert!(directory.join(LOCK_FILE).is_dir(), "nessuna riparazione");
}

#[cfg(windows)]
#[test]
fn a_lock_of_the_wrong_type_is_refused_without_repair() {
    let scratch = Scratch::new("win-wrong-type");
    let directory = scratch.path.join("totum");
    let root = scratch.root();
    fs::create_dir(directory.join(LOCK_FILE)).expect("directory creata");

    assert!(
        platform::lock(root).is_err(),
        "una directory non deve diventare un file di lock"
    );
    assert!(directory.join(LOCK_FILE).is_dir(), "nessuna riparazione");
}

#[cfg(unix)]
#[test]
fn a_lock_with_weakened_permissions_is_refused_without_repair() {
    let scratch = Scratch::new("wrong-mode");
    let path = scratch.path.join("totum").join(LOCK_FILE);

    let _forks = fork_window();
    drop(platform::lock(scratch.root()).expect("prima acquisizione"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("modalità impostata");

    match platform::lock(scratch.root()) {
        Err(PlatformError::WrongMode {
            expected: 0o600,
            actual: 0o644,
            ..
        }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(mode(&path), 0o644, "la modalità non viene riparata");
}

#[cfg(unix)]
#[test]
fn a_lock_owned_by_another_user_is_refused() {
    const NOBODY: u32 = 65534;

    let scratch = Scratch::new("wrong-owner");
    let path = scratch.path.join("totum").join(LOCK_FILE);

    let _forks = fork_window();
    drop(platform::lock(scratch.root()).expect("prima acquisizione"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).expect("modalità impostata");
    if std::os::unix::fs::chown(&path, Some(NOBODY), Some(NOBODY)).is_err() {
        eprintln!("cambio di proprietario non consentito: prova saltata");
        return;
    }

    match platform::lock(scratch.root()) {
        Err(PlatformError::WrongOwner {
            expected, actual, ..
        }) => {
            assert_eq!(actual, NOBODY.to_string());
            assert_ne!(expected, actual);
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(
        fs::symlink_metadata(&path).unwrap().uid(),
        NOBODY,
        "il proprietario non viene riparato"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_lock_is_refused_and_never_followed() {
    let scratch = Scratch::new("symlink");
    let directory = scratch.path.join("totum");
    let root = scratch.root();
    let outside = scratch.path.join("outside.lock");
    fs::write(&outside, b"altro").expect("bersaglio creato");
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).expect("modalità impostata");
    symlink(&outside, directory.join(LOCK_FILE)).expect("collegamento creato");

    match platform::lock(root) {
        Err(PlatformError::SymlinkRefused { path }) => {
            assert_eq!(path, directory.join(LOCK_FILE))
        }
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(fs::read(&outside).expect("bersaglio leggibile"), b"altro");
    assert_eq!(mode(&outside), 0o600, "il bersaglio non viene modificato");
    assert!(
        fs::symlink_metadata(directory.join(LOCK_FILE))
            .unwrap()
            .file_type()
            .is_symlink(),
        "il collegamento resta al suo posto"
    );
}

#[test]
fn an_existing_valid_lock_is_reused_without_rewriting_it() {
    let scratch = Scratch::new("reuse");
    let directory = scratch.path.join("totum");
    #[cfg(unix)]
    let path = directory.join(LOCK_FILE);
    let root = scratch.root();
    write(&root, LOCK_FILE, b"residuo");
    #[cfg(unix)]
    let before = inode(&path);

    drop(platform::lock(root).expect("acquisizione su file preesistente"));
    assert_eq!(read(&directory, LOCK_FILE), b"residuo");
    #[cfg(unix)]
    assert_eq!(inode(&path), before, "il file non viene sostituito");
}

// ---------------------------------------------------------------------------
// Esclusione fra processi reali
// ---------------------------------------------------------------------------

const CHILD_ENV: &str = "TOTUM_TEST_LOCK_SCENARIO";
const CHILD_DIR_ENV: &str = "TOTUM_TEST_LOCK_DIR";
const CHILD_TEST: &str = "lock_child";
const READY_DELAY: Duration = Duration::from_secs(30);
const CHILD_TIMEOUT: Duration = Duration::from_secs(60);

/// Processo figlio delle prove di esclusione.
///
/// Eseguito soltanto dal processo padre con `--ignored --exact`; senza la
/// variabile d'ambiente termina subito. `hold-store` resta in attesa del file
/// `stop` e termina normalmente; `hold-and-abort` termina con
/// `std::process::abort()`, senza unwinding, come un processo ucciso dal
/// sistema.
#[test]
#[ignore = "eseguito soltanto dai processi figli delle prove del lock"]
fn lock_child() {
    let Ok(scenario) = std::env::var(CHILD_ENV) else {
        return;
    };
    let base = PathBuf::from(std::env::var(CHILD_DIR_ENV).expect("directory del figlio"));
    let root = platform::prepare_root(&base.join("totum")).expect("radice del figlio preparabile");
    let locked = platform::lock(root).expect("lock del figlio");
    // La radice bloccata vive quanto lo Store; in `hold-and-abort` non viene
    // rilasciata prima di `abort`, così il test prova il rilascio operato dal
    // sistema alla morte del processo, non quello della chiusura del descrittore.
    let (_store, _) = Store::open(locked, COHERENT).expect("Store del figlio aperto");

    fs::write(base.join("ready"), b"").expect("segnale di prontezza");
    match scenario.as_str() {
        "hold-store" => {
            let deadline = Instant::now() + Duration::from_secs(120);
            while !base.join("stop").exists() {
                if Instant::now() >= deadline {
                    std::process::exit(3);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        "hold-and-abort" => std::process::abort(),
        other => panic!("scenario sconosciuto: {other}"),
    }
}

fn spawn_child(scenario: &str, base: &Path) -> Child {
    let executable = std::env::current_exe().expect("binario di prova");
    // `spawn` attende l'esito di `exec`: quando ritorna, il figlio ha chiuso i
    // descrittori marcati `CLOEXEC`, compreso ogni lock ereditato. Il mutex
    // impedisce che l'avvio cada fra un rilascio e la riacquisizione.
    let _forks = fork_window();
    Command::new(executable)
        .args(["--ignored", "--exact", CHILD_TEST])
        .env(CHILD_ENV, scenario)
        .env(CHILD_DIR_ENV, base)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("processo figlio avviato")
}

fn wait_ready(child: &mut Child, base: &Path) {
    let ready = base.join("ready");
    let deadline = Instant::now() + READY_DELAY;
    loop {
        if ready.exists() {
            return;
        }
        if let Some(status) = child.try_wait().expect("stato del figlio") {
            panic!("il figlio è terminato prima di essere pronto: {status}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("il figlio non è pronto entro {READY_DELAY:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_exit(child: &mut Child, scenario: &str) -> ExitStatus {
    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        match child.try_wait().expect("stato del figlio") {
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

#[test]
fn a_second_instance_is_refused_and_leaves_the_state_intact() {
    let scratch = Scratch::new("second-instance");
    let base = scratch.path.clone();
    let directory = base.join("totum");
    let root = scratch.root();
    write(&root, CONFIG_FILE, &valid(2));
    write(&root, CONFIG_BACKUP_FILE, &valid(1));
    write(&root, "notes.txt", b"nota");
    drop(root);

    let mut child = spawn_child("hold-store", &base);
    wait_ready(&mut child, &base);

    // La seconda istanza prepara e verifica il contenitore, poi tenta il lock:
    // non tocca configurazione, backup o temporanei prima dell'acquisizione.
    let second_root = platform::prepare_root(&directory).expect("contenitore verificato");
    write(&second_root, ".config.99999.0.tmp", b"temporaneo");
    write(&second_root, "config.corrupt.7.0", b"diagnostica");
    let before = snapshot(&directory);
    #[cfg(unix)]
    let lock_inode = inode(&directory.join(LOCK_FILE));

    match platform::lock(second_root) {
        Err(error) => is_already_running(error, &directory.join(LOCK_FILE)),
        Ok(_) => panic!("la seconda istanza non deve acquisire il lock"),
    }
    assert_eq!(snapshot(&directory), before, "nessuna modifica ai file");
    #[cfg(unix)]
    assert_eq!(
        inode(&directory.join(LOCK_FILE)),
        lock_inode,
        "il file di lock non viene sostituito"
    );

    fs::write(base.join("stop"), b"").expect("segnale di arresto");
    let status = wait_exit(&mut child, "hold-store");
    assert!(
        status.success(),
        "il detentore doveva terminare normalmente: {status}"
    );

    let (store, startup) = open_store(platform::prepare_root(&directory).expect("radice"));
    assert_eq!(
        store.config().revision(),
        2,
        "la generazione è quella caricata"
    );
    assert_eq!(
        startup.discarded_temporaries,
        vec![std::ffi::OsString::from(".config.99999.0.tmp")],
        "il temporaneo è sopravvissuto all'istanza rifiutata"
    );
    assert_eq!(read(&directory, CONFIG_FILE), valid(2));
    assert_eq!(read(&directory, CONFIG_BACKUP_FILE), valid(1));
    assert_eq!(read(&directory, "notes.txt"), b"nota");
    assert_eq!(read(&directory, "config.corrupt.7.0"), b"diagnostica");
    assert_eq!(store.save().expect("salvataggio").revision(), 3);
}

/// Una seconda istanza non può leggere né scrivere una radice che un'altra
/// istanza sta aprendo o ha aperto con una migrazione. Il detentore acquista
/// il lock prima della classificazione e lo trattiene per tutta l'apertura,
/// migrazione compresa; la pausa a metà migrazione non è osservabile
/// dall'esterno, ma l'esclusione vale per l'intera operazione.
#[test]
fn the_migration_runs_under_the_lock_that_excludes_a_second_instance() {
    let scratch = Scratch::new("migration-excluded");
    let base = scratch.path.clone();
    let directory = base.join("totum");
    let root = scratch.root();
    write(&root, CONFIG_FILE, br#"{"schemaVersion":0}"#);
    drop(root);

    // Il figlio apre lo Store: applica la migrazione con il lock acquisito e
    // poi resta in attesa del segnale di arresto.
    let mut child = spawn_child("hold-store", &base);
    wait_ready(&mut child, &base);

    let before = snapshot(&directory);
    match platform::lock(platform::prepare_root(&directory).expect("contenitore verificato")) {
        Err(error) => is_already_running(error, &directory.join(LOCK_FILE)),
        Ok(_) => panic!("la seconda istanza non deve acquisire il lock durante la migrazione"),
    }
    assert_eq!(
        snapshot(&directory),
        before,
        "nessuna modifica ai file della migrazione"
    );

    fs::write(base.join("stop"), b"").expect("segnale di arresto");
    let status = wait_exit(&mut child, "hold-store");
    assert!(
        status.success(),
        "il detentore doveva terminare normalmente: {status}"
    );

    let (store, startup) = open_store(platform::prepare_root(&directory).expect("radice"));
    assert_eq!(store.config().revision(), 1);
    assert!(matches!(
        startup.kind,
        totum_daemon::config::StartupKind::Normal { .. }
    ));
    assert!(startup.discarded_temporaries.is_empty());
    let mut migrated = valid(1);
    migrated.push(b'\n');
    assert_eq!(read(&directory, CONFIG_FILE), migrated);
    assert_eq!(
        read(&directory, CONFIG_BACKUP_FILE),
        br#"{"schemaVersion":0}"#,
        "il backup conserva i byte originali"
    );
}

#[test]
fn the_guard_lasts_until_the_store_is_dropped() {
    let scratch = Scratch::new("guard-lifetime");
    let base = scratch.path.clone();
    let directory = base.join("totum");

    let mut child = spawn_child("hold-store", &base);
    wait_ready(&mut child, &base);

    // Lo Store del figlio resta vivo: il lock è occupato per tutta la sua vita.
    let root = platform::prepare_root(&directory).expect("radice");
    match platform::lock(root) {
        Err(error) => is_already_running(error, &directory.join(LOCK_FILE)),
        Ok(_) => panic!("il guard non può essere disponibile mentre lo Store scrive"),
    }

    fs::write(base.join("stop"), b"").expect("segnale di arresto");
    assert!(wait_exit(&mut child, "hold-store").success());

    let (store, _) = open_store(platform::prepare_root(&directory).expect("radice"));
    assert_eq!(store.config().revision(), 0);
}

#[test]
fn a_dead_holder_is_replaced_without_deleting_the_lock_file() {
    let scratch = Scratch::new("holder-died");
    let base = scratch.path.clone();
    let directory = base.join("totum");

    let mut child = spawn_child("hold-and-abort", &base);
    wait_ready(&mut child, &base);
    let lock_path = directory.join(LOCK_FILE);
    assert!(lock_path.exists());
    #[cfg(unix)]
    let before = inode(&lock_path);

    let status = wait_exit(&mut child, "hold-and-abort");
    assert!(
        !status.success(),
        "il figlio doveva terminare senza unwinding"
    );
    assert_ne!(
        status.code(),
        Some(101),
        "un panic unwindato è un guasto della prova, non un arresto previsto"
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(6), "SIGABRT atteso: {status}");
        assert_eq!(
            inode(&lock_path),
            before,
            "il file di lock non viene cancellato"
        );
    }

    let (store, startup) = open_store(platform::prepare_root(&directory).expect("radice"));
    match startup.kind {
        totum_daemon::config::StartupKind::Normal { .. } => {}
        other => panic!("esito inatteso: {other:?}"),
    }
    assert_eq!(store.config().revision(), 0);
    assert_eq!(read(&directory, CONFIG_FILE), {
        let mut bytes = valid(0);
        bytes.push(b'\n');
        bytes
    });
    assert_eq!(store.save().expect("salvataggio").revision(), 1);
}

fn snapshot(directory: &Path) -> Vec<(String, Vec<u8>)> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).expect("directory leggibile") {
        let entry = entry.expect("voce leggibile");
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).expect("metadati disponibili");
        let contents = if metadata.is_file() && entry.file_name() != LOCK_FILE {
            fs::read(&path).expect("file leggibile")
        } else {
            Vec::new()
        };
        entries.push((entry.file_name().to_string_lossy().into_owned(), contents));
    }
    entries.sort();
    entries
}
