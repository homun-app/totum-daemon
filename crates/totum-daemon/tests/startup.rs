//! Prove del percorso di avvio operativo (D3 P8).
//!
//! Ogni prova avvia il binario `totum-daemon` reale con una radice di stato
//! reindirizzata da `HOME` (macOS), `XDG_STATE_HOME` (Linux) o `LOCALAPPDATA`
//! (Windows) verso una directory temporanea sotto `target/startup-tests`.
//! Nessun profilo reale viene letto o modificato. I processi residenti hanno
//! standard input nullo, termini massimi e una pulizia garantita: un test che
//! fallisce non lascia processi vivi.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use totum_daemon::config::CONFIG_FILE;
use totum_daemon::identity::{CONTROLLERS_FILE, IDENTITY_FILE};
use totum_daemon::journal::JOURNAL_FILE;
use totum_daemon::platform::{self, LOCK_FILE};

const IDENTITY_INITIAL: &[u8] = b"{\"schemaVersion\":1,\"state\":\"notInitialized\"}\n";
const CONTROLLERS_INITIAL: &[u8] =
    b"{\"schemaVersion\":1,\"grantRevision\":0,\"controllers\":[]}\n";
const CONFIG_INITIAL: &[u8] = b"{\"schemaVersion\":1,\"revision\":0}\n";
const BACKUP_FILE: &str = "config.json.prev";
const CORRUPT_BACKUP: &[u8] = b"{\"schemaVersion\":1,\"revision\":";

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(10);

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
            .join("target/startup-tests");
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

    fn base(&self) -> &Path {
        &self.path
    }

    fn state(&self) -> PathBuf {
        state_root(&self.path)
    }

    fn create_state(&self) -> PathBuf {
        let state = self.state();
        drop(platform::prepare_root(&state).expect("radice di stato preparabile"));
        state
    }

    fn write_file(&self, state: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let root = platform::prepare_root(state).expect("radice verificabile");
        let mut file = root.create_file(OsStr::new(name)).expect("file creabile");
        file.write_all(bytes).expect("contenuto scritto");
        file.sync_all().expect("contenuto sincronizzato");
        drop(file);
        drop(root);
        state.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn state_root(base: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        base.join("Library/Application Support/Totum")
    }
    #[cfg(target_os = "linux")]
    {
        base.join("state/totum")
    }
    #[cfg(windows)]
    {
        base.join("localappdata/Totum")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        base.join("totum")
    }
}

fn daemon_command(base: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_totum-daemon"));
    #[cfg(target_os = "macos")]
    {
        command.env("HOME", base);
    }
    #[cfg(target_os = "linux")]
    {
        command.env("HOME", base);
        command.env("XDG_STATE_HOME", base.join("state"));
    }
    #[cfg(windows)]
    {
        command.env("LOCALAPPDATA", base.join("localappdata"));
    }
    command
}

struct Running {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Running {
    fn spawn(base: &Path, label: &str) -> Self {
        let logs = base.join("logs");
        fs::create_dir_all(&logs).expect("directory dei log creabile");
        let stdout = logs.join(format!("{label}.out"));
        let stderr = logs.join(format!("{label}.err"));
        let child = daemon_command(base)
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                File::create(&stdout).expect("log standard output"),
            ))
            .stderr(Stdio::from(
                File::create(&stderr).expect("log standard error"),
            ))
            .spawn()
            .expect("daemon avviato");
        Running {
            child,
            stdout,
            stderr,
        }
    }

    #[cfg(unix)]
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn wait_ready(&mut self) -> String {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let stderr = read_text(&self.stderr);
            if stderr.contains(" pronto:") {
                return stderr;
            }
            if let Some(status) = self.child.try_wait().expect("stato del figlio") {
                panic!("il daemon è terminato prima della prontezza: {status}\nstderr:\n{stderr}");
            }
            if Instant::now() >= deadline {
                panic!("il daemon non è pronto entro {READY_TIMEOUT:?}\nstderr:\n{stderr}");
            }
            std::thread::sleep(POLL);
        }
    }

    fn stop(mut self) -> ExitStatus {
        terminate(&mut self.child);
        self.child.wait().expect("attesa del figlio")
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        // Terminazione predefinita del sistema: il prodotto non installa
        // gestori di segnale e il sistema rilascia il lock alla morte.
        if let Ok(pid) = i32::try_from(child.id()) {
            let _ = rustix::process::kill_process(
                rustix::process::Pid::from_raw(pid).expect("PID del figlio valido"),
                rustix::process::Signal::TERM,
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

struct Finished {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn run_to_exit(base: &Path, args: &[&str]) -> Finished {
    let mut child = daemon_command(base)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("processo avviato");
    let deadline = Instant::now() + EXIT_TIMEOUT;
    let status = loop {
        match child.try_wait().expect("stato del figlio") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("il processo non è terminato entro {EXIT_TIMEOUT:?}");
            }
            None => std::thread::sleep(POLL),
        }
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .expect("standard output del figlio")
        .read_to_string(&mut stdout)
        .expect("lettura dello standard output");
    child
        .stderr
        .take()
        .expect("standard error del figlio")
        .read_to_string(&mut stderr)
        .expect("lettura dello standard error");
    Finished {
        status,
        stdout,
        stderr,
    }
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn read(directory: &Path, name: &str) -> Vec<u8> {
    fs::read(directory.join(name)).expect("file leggibile")
}

fn names(directory: &Path) -> Vec<String> {
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

fn assert_no_temporaries(directory: &Path) {
    for name in names(directory) {
        assert!(
            !name.starts_with(".identity.")
                && !name.starts_with(".config.")
                && !name.starts_with(".journal."),
            "temporaneo residuo: {name}"
        );
    }
}

#[cfg(unix)]
fn inode(path: &Path) -> u64 {
    fs::symlink_metadata(path)
        .expect("metadati disponibili")
        .ino()
}

fn assert_refused(directory: &Path, finished: &Finished, fragment: &str) {
    assert_eq!(
        finished.status.code(),
        Some(1),
        "stderr:\n{}",
        finished.stderr
    );
    assert!(
        finished.stderr.contains(fragment),
        "atteso {fragment:?} in stderr:\n{}",
        finished.stderr
    );
    assert!(
        !finished.stderr.contains(" pronto:"),
        "nessuna prontezza dopo un errore:\n{}",
        finished.stderr
    );
    assert!(finished.stdout.is_empty(), "standard output vuoto");
    assert_no_temporaries(directory);
}

#[test]
fn first_run_creates_state_and_a_restart_does_not_rewrite_it() {
    let scratch = Scratch::new("first-run");
    let state = scratch.state();

    let mut first = Running::spawn(scratch.base(), "first");
    let stderr = first.wait_ready();
    let stdout = read_text(&first.stdout);
    assert!(stdout.is_empty(), "standard output vuoto:\n{stdout}");
    assert!(
        stderr.contains("prima esecuzione: creato identity.json"),
        "prima inizializzazione dell'identità:\n{stderr}"
    );
    assert!(
        stderr.contains("prima esecuzione: creato controllers.json"),
        "prima inizializzazione del registro:\n{stderr}"
    );
    assert!(
        stderr.contains("prima esecuzione: config.json creato alla revisione 0"),
        "prima esecuzione della configurazione:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("prima esecuzione: {JOURNAL_FILE} creato")),
        "prima esecuzione del journal:\n{stderr}"
    );
    assert!(
        !stderr.contains("bootstrap del journal interrotto"),
        "una radice nuova non ha residui di bootstrap:\n{stderr}"
    );
    #[cfg(windows)]
    assert!(
        stderr.contains("durabilità della voce di directory non confermata"),
        "su Windows la pubblicazione non è dichiarata durevole:\n{stderr}"
    );
    first.stop();

    assert_eq!(read(&state, IDENTITY_FILE), IDENTITY_INITIAL);
    assert_eq!(read(&state, CONTROLLERS_FILE), CONTROLLERS_INITIAL);
    assert_eq!(read(&state, CONFIG_FILE), CONFIG_INITIAL);
    assert!(state.join(JOURNAL_FILE).is_file());
    assert_eq!(
        names(&state),
        vec![
            CONFIG_FILE.to_owned(),
            CONTROLLERS_FILE.to_owned(),
            IDENTITY_FILE.to_owned(),
            JOURNAL_FILE.to_owned(),
            LOCK_FILE.to_owned(),
        ],
        "nessun temporaneo o file inatteso"
    );
    #[cfg(unix)]
    let before = inodes(&state);

    let mut second = Running::spawn(scratch.base(), "second");
    let stderr = second.wait_ready();
    assert!(
        !stderr.contains("prima esecuzione"),
        "il riavvio non è una prima esecuzione:\n{stderr}"
    );
    assert_eq!(read(&state, IDENTITY_FILE), IDENTITY_INITIAL);
    assert_eq!(read(&state, CONTROLLERS_FILE), CONTROLLERS_INITIAL);
    assert_eq!(read(&state, CONFIG_FILE), CONFIG_INITIAL);
    #[cfg(unix)]
    assert_eq!(inodes(&state), before, "il riavvio non riscrive i file");
    let status = second.stop();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(15), "SIGTERM predefinito: {status}");
    }
    #[cfg(not(unix))]
    let _ = status;
}

#[cfg(unix)]
fn inodes(directory: &Path) -> Vec<(String, u64)> {
    names(directory)
        .into_iter()
        .map(|name| {
            let value = inode(&directory.join(&name));
            (name, value)
        })
        .collect()
}

#[test]
fn an_interrupted_journal_bootstrap_is_reported_and_recovered() {
    let scratch = Scratch::new("journal-residue");
    let state = scratch.create_state();
    scratch.write_file(&state, IDENTITY_FILE, IDENTITY_INITIAL);
    scratch.write_file(&state, CONTROLLERS_FILE, CONTROLLERS_INITIAL);
    scratch.write_file(&state, CONFIG_FILE, CONFIG_INITIAL);
    scratch.write_file(&state, ".journal.4242.0.tmp", b"parziale");
    #[cfg(unix)]
    let config_inode = inode(&state.join(CONFIG_FILE));

    let mut running = Running::spawn(scratch.base(), "journal-residue");
    let stderr = running.wait_ready();
    assert!(
        stderr.contains(&format!(
            "bootstrap del journal interrotto: 1 temporanei scartati, {JOURNAL_FILE} creato"
        )),
        "residuo di bootstrap segnalato:\n{stderr}"
    );
    assert!(
        !stderr.contains("bootstrap del journal interrotto: 0"),
        "il conteggio è quello dei residui osservati:\n{stderr}"
    );
    assert_eq!(read(&state, IDENTITY_FILE), IDENTITY_INITIAL);
    assert_eq!(read(&state, CONTROLLERS_FILE), CONTROLLERS_INITIAL);
    assert_eq!(read(&state, CONFIG_FILE), CONFIG_INITIAL);
    #[cfg(unix)]
    assert_eq!(
        inode(&state.join(CONFIG_FILE)),
        config_inode,
        "la configurazione non viene riscritta"
    );
    assert!(state.join(JOURNAL_FILE).is_file());
    assert_no_temporaries(&state);
    running.stop();
}

#[test]
fn a_corrupt_journal_blocks_readiness_with_a_diagnosis() {
    let scratch = Scratch::new("journal-corrupt");
    let state = scratch.create_state();
    scratch.write_file(&state, IDENTITY_FILE, IDENTITY_INITIAL);
    scratch.write_file(&state, CONTROLLERS_FILE, CONTROLLERS_INITIAL);
    scratch.write_file(&state, CONFIG_FILE, CONFIG_INITIAL);
    let corrupt = b"non e un journal redb";
    scratch.write_file(&state, JOURNAL_FILE, corrupt);
    #[cfg(unix)]
    let config_inode = inode(&state.join(CONFIG_FILE));

    let finished = run_to_exit(scratch.base(), &[]);
    assert_eq!(
        finished.status.code(),
        Some(1),
        "uscita operativa, non un uso scorretto:\n{}",
        finished.stderr
    );
    assert!(
        finished.stderr.contains("avvio non riuscito"),
        "la diagnosi è un avvio non riuscito:\n{}",
        finished.stderr
    );
    assert!(
        finished.stderr.contains("apertura del journal fallita"),
        "la diagnosi nomina il journal:\n{}",
        finished.stderr
    );
    assert!(
        finished.stderr.contains("journal inutilizzabile"),
        "la causa è classificata, non un errore generico:\n{}",
        finished.stderr
    );
    assert!(
        !finished.stderr.contains(" pronto:"),
        "nessuna prontezza dopo un errore bloccante:\n{}",
        finished.stderr
    );
    assert_eq!(read(&state, JOURNAL_FILE), corrupt, "il file non è toccato");
    assert_eq!(read(&state, CONFIG_FILE), CONFIG_INITIAL);
    assert_eq!(read(&state, IDENTITY_FILE), IDENTITY_INITIAL);
    #[cfg(unix)]
    assert_eq!(inode(&state.join(CONFIG_FILE)), config_inode);
    assert_no_temporaries(&state);
}

#[test]
fn partial_initial_states_are_completed_without_rewriting_present_files() {
    for (label, present, missing, present_bytes, missing_bytes) in [
        (
            "identity-only",
            IDENTITY_FILE,
            CONTROLLERS_FILE,
            IDENTITY_INITIAL,
            CONTROLLERS_INITIAL,
        ),
        (
            "controllers-only",
            CONTROLLERS_FILE,
            IDENTITY_FILE,
            CONTROLLERS_INITIAL,
            IDENTITY_INITIAL,
        ),
    ] {
        let scratch = Scratch::new(label);
        let state = scratch.create_state();
        let path = scratch.write_file(&state, present, present_bytes);
        #[cfg(unix)]
        let before = inode(&path);
        #[cfg(not(unix))]
        let _ = path;

        let mut running = Running::spawn(scratch.base(), label);
        let stderr = running.wait_ready();
        assert_eq!(read(&state, present), present_bytes);
        assert_eq!(read(&state, missing), missing_bytes);
        assert_eq!(read(&state, CONFIG_FILE), CONFIG_INITIAL);
        assert!(stderr.contains(&format!("prima esecuzione: creato {missing}")));
        assert!(!stderr.contains(&format!("prima esecuzione: creato {present}")));
        #[cfg(unix)]
        assert_eq!(inode(&path), before, "{present} non viene riscritto");
        assert_no_temporaries(&state);
        running.stop();
    }
}

#[test]
fn incoherent_state_is_refused_without_creating_identity() {
    let scratch = Scratch::new("incoherent");
    let state = scratch.create_state();
    scratch.write_file(&state, CONFIG_FILE, CONFIG_INITIAL);

    let finished = run_to_exit(scratch.base(), &[]);
    assert_refused(&state, &finished, "stato incoerente");
    assert!(!state.join(IDENTITY_FILE).exists());
    assert!(!state.join(CONTROLLERS_FILE).exists());
    assert_eq!(read(&state, CONFIG_FILE), CONFIG_INITIAL);
}

#[test]
fn a_future_schema_is_refused_without_rewriting() {
    let scratch = Scratch::new("future");
    let state = scratch.create_state();
    scratch.write_file(&state, IDENTITY_FILE, IDENTITY_INITIAL);
    scratch.write_file(&state, CONTROLLERS_FILE, CONTROLLERS_INITIAL);
    let future = b"{\"schemaVersion\":2,\"revision\":0}\n";
    scratch.write_file(&state, CONFIG_FILE, future);

    let finished = run_to_exit(scratch.base(), &[]);
    assert_refused(&state, &finished, "schema 2 successivo a 1");
    assert_eq!(read(&state, CONFIG_FILE), future);
    assert_eq!(read(&state, IDENTITY_FILE), IDENTITY_INITIAL);
}

#[test]
fn non_initial_contents_are_refused_without_repair() {
    let scratch = Scratch::new("non-initial");
    let state = scratch.create_state();
    let non_initial = b"{\"schemaVersion\":1,\"grantRevision\":1,\"controllers\":[]}\n";
    scratch.write_file(&state, IDENTITY_FILE, IDENTITY_INITIAL);
    scratch.write_file(&state, CONTROLLERS_FILE, non_initial);

    let finished = run_to_exit(scratch.base(), &[]);
    assert_refused(&state, &finished, "non iniziale");
    assert_eq!(read(&state, CONTROLLERS_FILE), non_initial);
    assert!(!state.join(CONFIG_FILE).exists());
}

#[cfg(unix)]
#[test]
fn weakened_protections_are_refused_without_repair() {
    let scratch = Scratch::new("wrong-mode");
    let state = scratch.create_state();
    let path = scratch.write_file(&state, IDENTITY_FILE, IDENTITY_INITIAL);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("modalità indebolita");

    let finished = run_to_exit(scratch.base(), &[]);
    assert_refused(&state, &finished, "modalità errata");
    assert_eq!(read(&state, IDENTITY_FILE), IDENTITY_INITIAL);
    assert_eq!(
        fs::symlink_metadata(&path).unwrap().permissions().mode() & 0o7777,
        0o644,
        "la modalità non viene riparata"
    );
}

#[test]
fn a_corrupt_main_is_recovered_from_the_backup_at_startup() {
    let scratch = Scratch::new("recover");
    let state = scratch.create_state();
    scratch.write_file(&state, IDENTITY_FILE, IDENTITY_INITIAL);
    scratch.write_file(&state, CONTROLLERS_FILE, CONTROLLERS_INITIAL);
    let corrupt = b"{\"schemaVersion\":1,\"revision\":";
    scratch.write_file(&state, CONFIG_FILE, corrupt);
    scratch.write_file(
        &state,
        BACKUP_FILE,
        b"{\"schemaVersion\":1,\"revision\":3}\n",
    );

    let mut running = Running::spawn(scratch.base(), "recover");
    let stderr = running.wait_ready();
    assert!(
        stderr.contains("recupero: config.json ripristinato dal backup alla revisione 3"),
        "recupero segnalato:\n{stderr}"
    );
    assert!(
        stderr.contains("copia del principale precedente conservata come config.corrupt."),
        "copia del principale corrotto:\n{stderr}"
    );
    assert_eq!(
        read(&state, CONFIG_FILE),
        b"{\"schemaVersion\":1,\"revision\":3}\n"
    );
    assert!(
        names(&state)
            .iter()
            .any(|name| name.starts_with("config.corrupt."))
    );
    assert_no_temporaries(&state);
    running.stop();

    let mut restarted = Running::spawn(scratch.base(), "recover-restart");
    let stderr = restarted.wait_ready();
    assert!(
        !stderr.contains("recupero"),
        "nessun nuovo recupero:\n{stderr}"
    );
    restarted.stop();
}

#[test]
fn a_corrupt_backup_is_reported_without_repair() {
    let scratch = Scratch::new("corrupt-backup");
    let state = scratch.create_state();
    scratch.write_file(&state, IDENTITY_FILE, IDENTITY_INITIAL);
    scratch.write_file(&state, CONTROLLERS_FILE, CONTROLLERS_INITIAL);
    scratch.write_file(&state, CONFIG_FILE, CONFIG_INITIAL);
    scratch.write_file(&state, BACKUP_FILE, CORRUPT_BACKUP);
    let documents = |state: &Path| {
        [
            (IDENTITY_FILE, read(state, IDENTITY_FILE)),
            (CONTROLLERS_FILE, read(state, CONTROLLERS_FILE)),
            (CONFIG_FILE, read(state, CONFIG_FILE)),
            (BACKUP_FILE, read(state, BACKUP_FILE)),
        ]
    };
    let before = documents(&state);
    #[cfg(unix)]
    let before_inodes: Vec<(&str, u64)> = before
        .iter()
        .map(|(name, _)| (*name, inode(&state.join(name))))
        .collect();

    let mut running = Running::spawn(scratch.base(), "corrupt-backup");
    let stderr = running.wait_ready();
    let warning = stderr
        .find("config.json.prev inutilizzabile")
        .unwrap_or_else(|| panic!("avviso sul backup mancante:\n{stderr}"));
    let ready = stderr
        .find(" pronto:")
        .expect("una sola riga di prontezza nel log");
    assert!(warning < ready, "l'avviso precede la prontezza:\n{stderr}");
    assert!(
        stderr.contains("il backup non viene riparato"),
        "l'avviso dichiara che il backup non viene riparato:\n{stderr}"
    );
    assert!(
        running
            .child
            .try_wait()
            .expect("stato del figlio")
            .is_none(),
        "il processo resta residente dopo l'avviso:\n{stderr}"
    );
    running.stop();

    assert_eq!(documents(&state), before, "nessun documento modificato");
    #[cfg(unix)]
    assert_eq!(
        before
            .iter()
            .map(|(name, _)| (*name, inode(&state.join(name))))
            .collect::<Vec<_>>(),
        before_inodes,
        "nessun documento riscritto"
    );
    assert_no_temporaries(&state);
}

#[test]
fn a_valid_or_absent_backup_produces_no_warning() {
    for (label, backup) in [
        ("no-backup", None),
        (
            "valid-backup",
            Some(&b"{\"schemaVersion\":1,\"revision\":0}\n"[..]),
        ),
    ] {
        let scratch = Scratch::new(label);
        let state = scratch.create_state();
        scratch.write_file(&state, IDENTITY_FILE, IDENTITY_INITIAL);
        scratch.write_file(&state, CONTROLLERS_FILE, CONTROLLERS_INITIAL);
        scratch.write_file(&state, CONFIG_FILE, CONFIG_INITIAL);
        if let Some(bytes) = backup {
            scratch.write_file(&state, BACKUP_FILE, bytes);
        }

        let mut running = Running::spawn(scratch.base(), label);
        let stderr = running.wait_ready();
        assert!(
            !stderr.contains("config.json.prev inutilizzabile"),
            "nessun avviso con backup {label}:\n{stderr}"
        );
        running.stop();

        if let Some(bytes) = backup {
            assert_eq!(read(&state, BACKUP_FILE), bytes, "il backup resta intatto");
        }
        assert!(!state.join(BACKUP_FILE).exists() || backup.is_some());
        assert_no_temporaries(&state);
    }
}

#[test]
fn a_previous_schema_is_migrated_at_startup() {
    let scratch = Scratch::new("migrate");
    let state = scratch.create_state();
    scratch.write_file(&state, IDENTITY_FILE, IDENTITY_INITIAL);
    scratch.write_file(&state, CONTROLLERS_FILE, CONTROLLERS_INITIAL);
    scratch.write_file(&state, CONFIG_FILE, b"{\"schemaVersion\":0}");

    let mut running = Running::spawn(scratch.base(), "migrate");
    let stderr = running.wait_ready();
    assert!(
        stderr.contains(
            "migrazione: config.json trasformato dallo schema 0 alla revisione 1 (revisione precedente 0)"
        ),
        "migrazione segnalata:\n{stderr}"
    );
    assert_eq!(
        read(&state, CONFIG_FILE),
        b"{\"schemaVersion\":1,\"revision\":1}\n"
    );
    assert_eq!(
        read(&state, BACKUP_FILE),
        b"{\"schemaVersion\":0}",
        "il backup conserva i byte originali"
    );
    assert_no_temporaries(&state);
    running.stop();
}

// Esclusione fra due istanze dello stesso account sulla stessa radice: non è
// una prova fra account, che richiede l'harness TB-08 in CI.
#[test]
fn a_second_instance_is_excluded_and_the_lock_is_reacquired_after_termination() {
    let scratch = Scratch::new("second-instance");
    let state = scratch.create_state();
    scratch.write_file(&state, IDENTITY_FILE, IDENTITY_INITIAL);
    scratch.write_file(&state, CONTROLLERS_FILE, CONTROLLERS_INITIAL);
    scratch.write_file(&state, CONFIG_FILE, CONFIG_INITIAL);

    let mut first = Running::spawn(scratch.base(), "holder");
    first.wait_ready();
    let before = snapshot(&state);

    let second = run_to_exit(scratch.base(), &[]);
    assert_refused(&state, &second, "un'altra istanza detiene il lock");
    assert_eq!(
        snapshot(&state),
        before,
        "la seconda istanza non modifica nulla"
    );

    first.stop();
    #[cfg(unix)]
    let journal_before = inode(&state.join(JOURNAL_FILE));
    let mut replacement = Running::spawn(scratch.base(), "replacement");
    replacement.wait_ready();
    assert_eq!(
        snapshot(&state),
        before,
        "la nuova istanza non riscrive nulla"
    );
    #[cfg(unix)]
    assert_eq!(
        inode(&state.join(JOURNAL_FILE)),
        journal_before,
        "il journal viene riaperto in loco, non pubblicato di nuovo"
    );
    replacement.stop();
}

/// Un secondo daemon che incontra una radice con protezioni non conformi si
/// ferma con l'uscita operativa, prima del lock e della prontezza. Con un solo
/// account disponibile la non conformità osservabile è quella delle protezioni:
/// un vero `WrongOwner` richiede privilegi e resta coperto dall'harness TB-08
/// in CI.
#[cfg(unix)]
#[test]
fn a_second_daemon_refuses_a_root_with_foreign_protections_before_the_lock() {
    let first_home = Scratch::new("foreign-root");
    let root = first_home.create_state();
    first_home.write_file(&root, IDENTITY_FILE, IDENTITY_INITIAL);
    first_home.write_file(&root, CONTROLLERS_FILE, CONTROLLERS_INITIAL);
    first_home.write_file(&root, CONFIG_FILE, CONFIG_INITIAL);

    let mut daemon = Running::spawn(first_home.base(), "foreign-holder");
    daemon.wait_ready();

    // La radice perde la conformità delle protezioni dopo che il primo daemon
    // le ha verificate: il residente non ripara e il secondo deve fermarsi
    // sulla verifica, che precede il lock.
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755))
        .expect("protezione della radice indebolita");
    let before = snapshot(&root);

    let second = run_to_exit(first_home.base(), &[]);
    assert_refused(&root, &second, "modalità errata");
    assert_eq!(
        second.status.code(),
        Some(1),
        "uscita operativa 1, non un uso scorretto, un segnale o un timeout:\n{}",
        second.stderr
    );
    assert!(
        second.stderr.contains("avvio non riuscito"),
        "il rifiuto è segnalato come avvio non riuscito:\n{}",
        second.stderr
    );
    assert!(
        second.stderr.contains(&root.display().to_string()),
        "la radice rifiutata è nominata nella diagnosi:\n{}",
        second.stderr
    );
    assert!(
        !second.stderr.contains("detiene il lock"),
        "il rifiuto delle protezioni precede qualunque tentativo sul lock:\n{}",
        second.stderr
    );
    assert_eq!(
        snapshot(&root),
        before,
        "la radice estranea non viene riparata né modificata"
    );
    assert_eq!(
        fs::symlink_metadata(&root)
            .expect("metadati della radice indebolita")
            .permissions()
            .mode()
            & 0o7777,
        0o755,
        "la modalità trovata resta tale dopo il rifiuto"
    );
    assert!(
        matches!(daemon.child.try_wait(), Ok(None)),
        "il primo daemon resta residente: il tentativo non lo ha disturbato"
    );
    daemon.stop();
}

#[test]
fn version_and_misuse_do_not_create_state() {
    let scratch = Scratch::new("arguments");

    let version = run_to_exit(scratch.base(), &["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(
        version.stdout,
        format!("totum-daemon {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(version.stderr.is_empty());

    let misuse = run_to_exit(scratch.base(), &["--serve"]);
    assert_eq!(misuse.status.code(), Some(2));
    assert!(misuse.stdout.is_empty());
    assert!(misuse.stderr.contains("Uso: totum-daemon"));

    let extra = run_to_exit(scratch.base(), &["--version", "now"]);
    assert_eq!(extra.status.code(), Some(2));
    assert!(extra.stdout.is_empty());

    assert!(
        names(scratch.base()).is_empty(),
        "argomenti e versione non lasciano stato: {:?}",
        names(scratch.base())
    );
}

#[cfg(unix)]
#[test]
fn the_resident_process_opens_no_network_sockets() {
    let scratch = Scratch::new("no-listener");
    let mut running = Running::spawn(scratch.base(), "resident");
    running.wait_ready();

    match network_sockets(running.pid()) {
        None => eprintln!("lsof non disponibile: prova del listener saltata"),
        Some(entries) => assert!(
            entries.is_empty(),
            "socket di rete aperti dal processo residente: {entries:?}"
        ),
    }
    running.stop();
}

#[cfg(unix)]
fn network_sockets(pid: u32) -> Option<Vec<String>> {
    let output = Command::new("lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-i"])
        .output()
        .ok()?;
    match output.status.code() {
        Some(0) => Some(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .skip(1)
                .map(str::to_owned)
                .collect(),
        ),
        Some(1) if output.stdout.is_empty() => Some(Vec::new()),
        _ => None,
    }
}

fn snapshot(directory: &Path) -> Vec<(String, Vec<u8>)> {
    names(directory)
        .into_iter()
        .map(|name| {
            let path = directory.join(&name);
            // `journal.redb` è posseduto dal motore: una riapertura dopo una
            // terminazione non pulita può riscrivere pagine interne. Contano
            // il percorso e l'inode, non i byte; per gli altri file i byte
            // restano confrontabili.
            let compared = name != LOCK_FILE && name != JOURNAL_FILE;
            let bytes = if compared && fs::symlink_metadata(&path).unwrap().is_file() {
                fs::read(&path).expect("file leggibile")
            } else {
                Vec::new()
            };
            (name, bytes)
        })
        .collect()
}
