//! Sonda diagnostica del VFS Windows standard di SQLite su dati sintetici.
#![cfg(windows)]

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use totum_daemon::platform::{self, LockedRoot, PlatformError};

#[link(name = "winsqlite3")]
unsafe extern "C" {
    fn sqlite3_open(name: *const c_char, db: *mut *mut c_void) -> c_int;
    fn sqlite3_close(db: *mut c_void) -> c_int;
    fn sqlite3_libversion() -> *const c_char;
    fn sqlite3_exec(
        db: *mut c_void,
        sql: *const c_char,
        callback: Option<
            unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int,
        >,
        context: *mut c_void,
        error: *mut *mut c_char,
    ) -> c_int;
}

type Rows = Vec<Vec<String>>;

unsafe extern "C" fn collect(
    context: *mut c_void,
    count: c_int,
    values: *mut *mut c_char,
    _: *mut *mut c_char,
) -> c_int {
    let rows = unsafe { &mut *context.cast::<Rows>() };
    let mut row = Vec::new();
    for pos in 0..count as usize {
        let value = unsafe { *values.add(pos) };
        if value.is_null() {
            return 1;
        }
        row.push(
            unsafe { CStr::from_ptr(value) }
                .to_string_lossy()
                .into_owned(),
        );
    }
    rows.push(row);
    0
}

struct Db {
    handle: *mut c_void,
    _lock: LockedRoot,
}

impl Db {
    fn open(root: &Path) -> Self {
        let lock = platform::lock(platform::prepare_root(root).unwrap()).unwrap();
        let name = CString::new(root.join("spike.db").to_str().unwrap()).unwrap();
        let mut handle = std::ptr::null_mut();
        let rc = unsafe { sqlite3_open(name.as_ptr(), &mut handle) };
        if rc != 0 && !handle.is_null() {
            unsafe { sqlite3_close(handle) };
        }
        assert_eq!(rc, 0, "sqlite3_open");
        Self {
            handle,
            _lock: lock,
        }
    }

    fn exec(&self, sql: &str) -> Rows {
        let sql = CString::new(sql).unwrap();
        let mut rows = Rows::new();
        let rc = unsafe {
            sqlite3_exec(
                self.handle,
                sql.as_ptr(),
                Some(collect),
                (&mut rows as *mut Rows).cast(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "SQLite: {sql:?}");
        rows
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        assert_eq!(unsafe { sqlite3_close(self.handle) }, 0);
    }
}

#[test]
#[ignore = "eseguito dal test padre"]
fn diagnostic_child() {
    let Ok(root) = std::env::var("TOTUM_D4_PROBE_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let mode = std::env::var("TOTUM_D4_PROBE_MODE").unwrap();
    if mode == "probe" {
        let code = match platform::prepare_root(root).and_then(platform::lock) {
            Ok(_) => 10,
            Err(PlatformError::AlreadyRunning { .. }) => 11,
            Err(error) => {
                eprintln!("{error}");
                12
            }
        };
        std::process::exit(code);
    }
    let db = Db::open(root);
    assert_eq!(db.exec("PRAGMA journal_mode=DELETE"), [["delete"]]);
    db.exec("PRAGMA synchronous=EXTRA; PRAGMA cache_size=4; CREATE TABLE events(id INTEGER PRIMARY KEY, payload TEXT NOT NULL); INSERT INTO events VALUES(0,'seed'); BEGIN IMMEDIATE; WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<8) INSERT INTO events SELECT i,printf('batch-%d',i) FROM n;");
    if mode == "after" {
        db.exec("COMMIT");
    } else {
        assert!(root.join("spike.db-journal").is_file());
    }
    let mut ready = fs::File::create(root.parent().unwrap().join("ready")).unwrap();
    ready.write_all(b"ready").unwrap();
    ready.sync_all().unwrap();
    loop {
        std::thread::park();
    }
}

fn run_child(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", "diagnostic_child", "--nocapture"])
        .env("TOTUM_D4_PROBE_ROOT", root)
        .env("TOTUM_D4_PROBE_MODE", mode)
        .stdin(Stdio::null());
    command
}

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn wait_for(child: &mut Child, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "figlio terminato prima della barriera"
        );
        assert!(Instant::now() < deadline, "timeout alla barriera");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn check_lock(root: &Path, code: i32) {
    let mut child = Child(run_child(root, "probe").spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert_eq!(status.code(), Some(code));
            return;
        }
        assert!(Instant::now() < deadline, "timeout sonda lock");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn windows_native_sqlite_process_recovery_and_totum_lock() {
    assert!(!platform::is_elevated().unwrap());
    println!(
        "Windows SQLite {}",
        unsafe { CStr::from_ptr(sqlite3_libversion()) }.to_string_lossy()
    );
    for mode in ["before", "after"] {
        let scratch = super_scratch::Scratch::new(mode);
        let root = scratch.0.join("totum");
        let mut child = Child(run_child(&root, mode).spawn().unwrap());
        wait_for(&mut child, || {
            fs::read(scratch.0.join("ready")).is_ok_and(|bytes| bytes == b"ready")
        });
        check_lock(&root, 11);
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "processo già terminato"
        );
        child.0.kill().unwrap();
        let status = child.0.wait().unwrap();
        assert!(!status.success(), "arresto non osservato");
        check_lock(&root, 10);
        let db = Db::open(&root);
        assert_eq!(db.exec("PRAGMA integrity_check"), [["ok"]]);
        let mut expected = vec![vec!["0".to_owned(), "seed".to_owned()]];
        if mode == "after" {
            expected.extend((1..=8).map(|id| vec![id.to_string(), format!("batch-{id}")]));
        }
        assert_eq!(
            db.exec("SELECT id,payload FROM events ORDER BY id"),
            expected
        );
        assert!(!root.join("spike.db-journal").exists());
        println!(
            "{mode}: lock escluso durante la connessione, riacquisito dopo arresto, {} record integri",
            expected.len()
        );
    }
}

mod super_scratch {
    use std::fs;
    use std::path::{Path, PathBuf};
    pub struct Scratch(pub PathBuf);
    impl Scratch {
        pub fn new(label: &str) -> Self {
            let base = Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap()
                .join("target/d4-windows-probe");
            fs::create_dir_all(&base).unwrap();
            let path = base.join(format!("{label}-{}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("pulizia fixture");
        }
    }
}
