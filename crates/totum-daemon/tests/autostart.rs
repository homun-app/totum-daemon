//! Prove delle primitive di avvio automatico su directory temporanee.
//!
//! Nessun test usa il profilo reale: i layout sono sempre espliciti e puntano a
//! uno scratch sotto la directory temporanea. Le prove del meccanismo systemd
//! girano anche su macOS perché il backend è lo stesso codice Unix; la
//! disponibilità di systemd è un valore iniettato e non viene mai interrogato
//! il sistema.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use totum_daemon::autostart::{
    Autostart, AutostartError, Durability, InstallChange, Layout, Mechanism, RemoveChange, Status,
};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "totum-autostart-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn owner(path: &Path) -> u32 {
    fs::metadata(path).unwrap().uid()
}

fn write_file(path: &Path, content: &str, mode_bits: u32) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode_bits)).unwrap();
}

fn assert_no_temporary(directory: &Path) {
    for entry in fs::read_dir(directory).unwrap() {
        let name = entry.unwrap().file_name();
        assert!(
            !name.to_string_lossy().starts_with(".autostart."),
            "temporaneo residuo: {}",
            name.to_string_lossy()
        );
    }
}

fn launch_layout(scratch: &Scratch, directory: &str) -> Autostart {
    Autostart::new(Layout::launch_agent(scratch.join(directory)))
}

fn systemd_layout(scratch: &Scratch, directory: &str, available: bool) -> Autostart {
    Autostart::new(Layout::systemd_user(scratch.join(directory), available))
}

fn plist_path(scratch: &Scratch, directory: &str) -> PathBuf {
    scratch.join(directory).join("app.homun.totum.daemon.plist")
}

fn unit_path(scratch: &Scratch, directory: &str) -> PathBuf {
    scratch.join(directory).join("totum-daemon.service")
}

fn wants_path(scratch: &Scratch, directory: &str) -> PathBuf {
    scratch
        .join(directory)
        .join("default.target.wants")
        .join("totum-daemon.service")
}

#[test]
fn launch_agent_is_idempotent_and_removable() {
    let scratch = Scratch::new();
    let autostart = launch_layout(&scratch, "LaunchAgents");
    let binary = PathBuf::from("/Applications/Totum daemon/100% $HOME/dæmon/totum-daemon");

    assert_eq!(autostart.status().unwrap(), Status::Absent);
    assert!(!scratch.join("LaunchAgents").exists());

    let report = autostart.install(&binary).unwrap();
    assert_eq!(report.entry, InstallChange::Created);
    assert_eq!(report.link, None);
    assert_eq!(report.durability, Durability::Confirmed);

    let file = plist_path(&scratch, "LaunchAgents");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(owner(&file), owner(scratch.path()));
    let content = fs::read_to_string(&file).unwrap();
    assert!(content.contains("RunAtLoad"));
    assert!(!content.contains("KeepAlive"));
    assert!(content.contains("/Applications/Totum daemon/100% $HOME/dæmon/totum-daemon"));
    assert_eq!(
        autostart.status().unwrap(),
        Status::Registered {
            program: binary.clone()
        }
    );

    let before = fs::read(&file).unwrap();
    let again = autostart.install(&binary).unwrap();
    assert_eq!(again.entry, InstallChange::Unchanged);
    assert_eq!(again.durability, Durability::Unknown);
    assert_eq!(fs::read(&file).unwrap(), before);
    let entries: Vec<_> = fs::read_dir(scratch.join("LaunchAgents"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        entries,
        vec![std::ffi::OsString::from("app.homun.totum.daemon.plist")]
    );
    assert_no_temporary(&scratch.join("LaunchAgents"));

    let removed = autostart.remove().unwrap();
    assert_eq!(removed.entry, RemoveChange::Removed);
    assert_eq!(removed.durability, Durability::Confirmed);
    assert!(!file.exists());
    assert_eq!(autostart.status().unwrap(), Status::Absent);

    let removed_again = autostart.remove().unwrap();
    assert_eq!(removed_again.entry, RemoveChange::AlreadyAbsent);
    assert_eq!(removed_again.durability, Durability::Unknown);
}

#[test]
fn launch_agent_updates_its_own_stale_registration_without_duplicating() {
    let scratch = Scratch::new();
    let autostart = launch_layout(&scratch, "LaunchAgents");
    let first = PathBuf::from("/opt/Totum/totum-daemon");
    let second = PathBuf::from("/opt/Totum 2/totum-daemon");

    autostart.install(&first).unwrap();
    let report = autostart.install(&second).unwrap();
    assert_eq!(report.entry, InstallChange::Updated);
    assert_eq!(
        autostart.status().unwrap(),
        Status::Registered { program: second }
    );
    assert_eq!(
        fs::read_dir(scratch.join("LaunchAgents")).unwrap().count(),
        1
    );
    assert_no_temporary(&scratch.join("LaunchAgents"));
}

#[test]
fn launch_agent_never_touches_foreign_content_or_unsafe_objects() {
    let scratch = Scratch::new();
    let directory = scratch.join("LaunchAgents");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let file = plist_path(&scratch, "LaunchAgents");
    write_file(
        &file,
        "<?xml version=\"1.0\"?><plist>foreign</plist>",
        0o600,
    );

    let autostart = launch_layout(&scratch, "LaunchAgents");
    assert!(matches!(
        autostart.status().unwrap(),
        Status::Incompatible { .. }
    ));
    let failure = autostart
        .install(Path::new("/opt/Totum/totum-daemon"))
        .unwrap_err();
    assert!(matches!(
        *failure.error,
        AutostartError::IncompatibleObject { .. }
    ));
    assert!(matches!(
        *autostart.remove().unwrap_err().error,
        AutostartError::IncompatibleObject { .. }
    ));
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "<?xml version=\"1.0\"?><plist>foreign</plist>"
    );

    fs::remove_file(&file).unwrap();
    symlink("/etc/passwd", &file).unwrap();
    assert!(matches!(
        autostart.status().unwrap(),
        Status::Incompatible { .. }
    ));
    assert!(
        autostart
            .install(Path::new("/opt/Totum/totum-daemon"))
            .is_err()
    );
    assert!(autostart.remove().is_err());
    assert_eq!(fs::read_link(&file).unwrap(), PathBuf::from("/etc/passwd"));
}

#[test]
fn launch_agent_refuses_an_unsafe_directory_without_repair() {
    let scratch = Scratch::new();
    let directory = scratch.join("LaunchAgents");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();

    let autostart = launch_layout(&scratch, "LaunchAgents");
    assert!(matches!(
        autostart.status().unwrap(),
        Status::Incompatible { .. }
    ));
    let failure = autostart
        .install(Path::new("/opt/Totum/totum-daemon"))
        .unwrap_err();
    assert!(matches!(
        *failure.error,
        AutostartError::UnsafeObject { .. }
    ));
    assert_eq!(mode(&directory), 0o777);
    assert!(!plist_path(&scratch, "LaunchAgents").exists());
}

#[test]
fn launch_agent_rejects_objects_with_wrong_mode_without_rewriting() {
    let scratch = Scratch::new();
    let autostart = launch_layout(&scratch, "LaunchAgents");
    let binary = PathBuf::from("/opt/Totum/totum-daemon");
    autostart.install(&binary).unwrap();
    let file = plist_path(&scratch, "LaunchAgents");
    let before = fs::read(&file).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(matches!(
        autostart.status().unwrap(),
        Status::Incompatible { .. }
    ));
    assert!(autostart.install(&binary).is_err());
    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(mode(&file), 0o644);
    assert!(autostart.remove().is_err());
    assert!(file.exists());
}

#[test]
fn launch_agent_does_not_change_a_pre_existing_standard_directory() {
    let scratch = Scratch::new();
    let directory = scratch.join("LaunchAgents");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();

    let autostart = launch_layout(&scratch, "LaunchAgents");
    autostart
        .install(Path::new("/opt/Totum/totum-daemon"))
        .unwrap();
    assert_eq!(mode(&directory), 0o755);
}

#[test]
fn systemd_registers_the_unit_and_relative_link_without_systemctl() {
    let scratch = Scratch::new();
    let autostart = systemd_layout(&scratch, "config/systemd/user", true);
    let binary = PathBuf::from("/opt/Totum 100% $HOME/a\\b\"c;d/dæmon/totum-daemon");

    assert_eq!(autostart.status().unwrap(), Status::Absent);
    let report = autostart.install(&binary).unwrap();
    assert_eq!(report.entry, InstallChange::Created);
    assert_eq!(report.link, Some(InstallChange::Created));
    assert_eq!(report.durability, Durability::Confirmed);

    let unit = unit_path(&scratch, "config/systemd/user");
    assert_eq!(mode(&unit), 0o600);
    assert_eq!(owner(&unit), owner(scratch.path()));
    let content = fs::read_to_string(&unit).unwrap();
    assert!(content.starts_with("[Unit]"));
    assert!(content.contains("WantedBy=default.target"));
    assert!(content.contains("ExecStart=:\""));
    assert!(content.contains("100%%"));
    assert!(content.contains("$HOME"));
    assert!(!content.contains("$$HOME"));
    assert!(content.contains("a\\\\b"));
    assert!(content.contains("\\\"c"));
    assert_eq!(
        fs::read_link(wants_path(&scratch, "config/systemd/user")).unwrap(),
        PathBuf::from("../totum-daemon.service")
    );
    assert_eq!(mode(&scratch.join("config/systemd/user")), 0o700);
    assert_eq!(
        mode(&scratch.join("config/systemd/user/default.target.wants")),
        0o700
    );
    assert_eq!(
        autostart.status().unwrap(),
        Status::Registered {
            program: binary.clone()
        }
    );

    let again = autostart.install(&binary).unwrap();
    assert_eq!(again.entry, InstallChange::Unchanged);
    assert_eq!(again.link, Some(InstallChange::Unchanged));
    assert_eq!(again.durability, Durability::Unknown);
    assert_eq!(
        fs::read_dir(scratch.join("config/systemd/user"))
            .unwrap()
            .count(),
        2
    );
    assert_no_temporary(&scratch.join("config/systemd/user"));

    let removed = autostart.remove().unwrap();
    assert_eq!(removed.entry, RemoveChange::Removed);
    assert_eq!(removed.link, Some(RemoveChange::Removed));
    assert!(!unit.exists());
    assert!(!wants_path(&scratch, "config/systemd/user").exists());
    assert!(
        scratch
            .join("config/systemd/user/default.target.wants")
            .exists()
    );
    assert_eq!(autostart.status().unwrap(), Status::Absent);
    let removed_again = autostart.remove().unwrap();
    assert_eq!(removed_again.entry, RemoveChange::AlreadyAbsent);
    assert_eq!(removed_again.link, Some(RemoveChange::AlreadyAbsent));
}

#[test]
fn systemd_is_unavailable_without_systemd_and_writes_nothing() {
    let scratch = Scratch::new();
    let autostart = systemd_layout(&scratch, "config/systemd/user", false);
    let binary = PathBuf::from("/opt/Totum/totum-daemon");

    assert!(matches!(
        autostart.status().unwrap(),
        Status::Unavailable { .. }
    ));
    let failure = autostart.install(&binary).unwrap_err();
    assert!(matches!(*failure.error, AutostartError::Unavailable { .. }));
    assert_eq!(failure.report.entry, InstallChange::NotAttempted);
    assert_eq!(failure.report.link, Some(InstallChange::NotAttempted));
    assert!(!scratch.join("config").exists());

    let removed = autostart.remove().unwrap();
    assert_eq!(removed.entry, RemoveChange::AlreadyAbsent);
    assert_eq!(removed.link, Some(RemoveChange::AlreadyAbsent));
}

#[test]
fn systemd_never_touches_a_foreign_unit_or_link() {
    let scratch = Scratch::new();
    let directory = scratch.join("config/systemd/user");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let unit = unit_path(&scratch, "config/systemd/user");
    write_file(&unit, "[Service]\nExecStart=/bin/other\n", 0o600);

    let autostart = systemd_layout(&scratch, "config/systemd/user", true);
    assert!(matches!(
        autostart.status().unwrap(),
        Status::Incompatible { .. }
    ));
    let failure = autostart
        .install(Path::new("/opt/Totum/totum-daemon"))
        .unwrap_err();
    assert!(matches!(
        *failure.error,
        AutostartError::IncompatibleObject { .. }
    ));
    assert!(autostart.remove().is_err());
    assert_eq!(
        fs::read_to_string(&unit).unwrap(),
        "[Service]\nExecStart=/bin/other\n"
    );
    assert!(!directory.join("default.target.wants").exists());

    fs::remove_file(&unit).unwrap();
    let wants = directory.join("default.target.wants");
    fs::create_dir(&wants).unwrap();
    fs::set_permissions(&wants, fs::Permissions::from_mode(0o700)).unwrap();
    symlink("/etc/passwd", wants.join("totum-daemon.service")).unwrap();
    assert!(matches!(
        autostart.status().unwrap(),
        Status::Incompatible { .. }
    ));
    assert!(
        autostart
            .install(Path::new("/opt/Totum/totum-daemon"))
            .is_err()
    );
    assert!(autostart.remove().is_err());
    assert!(!unit.exists());
    assert_eq!(
        fs::read_link(wants.join("totum-daemon.service")).unwrap(),
        PathBuf::from("/etc/passwd")
    );
}

#[test]
fn systemd_reports_partial_install_when_the_link_cannot_be_created() {
    let scratch = Scratch::new();
    let directory = scratch.join("config/systemd/user");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let wants = directory.join("default.target.wants");
    fs::create_dir(&wants).unwrap();
    fs::set_permissions(&wants, fs::Permissions::from_mode(0o500)).unwrap();

    let autostart = systemd_layout(&scratch, "config/systemd/user", true);
    let binary = PathBuf::from("/opt/Totum/totum-daemon");
    let failure = autostart.install(&binary).unwrap_err();
    assert_eq!(failure.report.entry, InstallChange::Created);
    assert_eq!(failure.report.link, Some(InstallChange::NotAttempted));
    assert_eq!(failure.report.durability, Durability::Unknown);
    assert!(unit_path(&scratch, "config/systemd/user").exists());
    assert!(!wants.join("totum-daemon.service").exists());
    assert_no_temporary(&directory);

    fs::set_permissions(&wants, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        autostart.status().unwrap(),
        Status::Incompatible { .. }
    ));
    let completed = autostart.install(&binary).unwrap();
    assert_eq!(completed.entry, InstallChange::Unchanged);
    assert_eq!(completed.link, Some(InstallChange::Created));
    assert_eq!(
        autostart.status().unwrap(),
        Status::Registered { program: binary }
    );
    let removed = autostart.remove().unwrap();
    assert_eq!(removed.entry, RemoveChange::Removed);
    assert_eq!(removed.link, Some(RemoveChange::Removed));
}

#[test]
fn systemd_accepts_an_absolute_link_to_its_own_unit() {
    let scratch = Scratch::new();
    let autostart = systemd_layout(&scratch, "config/systemd/user", true);
    let binary = PathBuf::from("/opt/Totum/totum-daemon");
    autostart.install(&binary).unwrap();

    let link = wants_path(&scratch, "config/systemd/user");
    fs::remove_file(&link).unwrap();
    let unit = unit_path(&scratch, "config/systemd/user");
    symlink(&unit, &link).unwrap();
    assert!(matches!(
        autostart.status().unwrap(),
        Status::Registered { .. }
    ));
    let report = autostart.install(&binary).unwrap();
    assert_eq!(report.entry, InstallChange::Unchanged);
    assert_eq!(report.link, Some(InstallChange::Unchanged));
}

#[test]
fn systemd_does_not_change_pre_existing_directories() {
    let scratch = Scratch::new();
    let directory = scratch.join("config/systemd/user");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(scratch.join("config"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(
        scratch.join("config/systemd"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();

    let autostart = systemd_layout(&scratch, "config/systemd/user", true);
    autostart
        .install(Path::new("/opt/Totum/totum-daemon"))
        .unwrap();
    assert_eq!(mode(&scratch.join("config")), 0o755);
    assert_eq!(mode(&scratch.join("config/systemd")), 0o755);
    assert_eq!(mode(&directory), 0o755);
    assert_eq!(mode(&directory.join("default.target.wants")), 0o700);
}

#[test]
fn status_is_read_only_and_does_not_create_directories() {
    let scratch = Scratch::new();
    // Le due asserzioni sulla radice assente dimostrano qualcosa solo se la
    // creazione fallirebbe davvero: un ambiente che già possiede quelle
    // directory renderebbe la prova silenziosamente vuota.
    assert!(
        !scratch.join("missing").exists() && !scratch.join("missing2").exists(),
        "l'ambiente di prova non deve possedere già le directory della radice assente"
    );
    let autostart = launch_layout(&scratch, "missing/LaunchAgents");
    assert_eq!(autostart.status().unwrap(), Status::Absent);
    assert!(!scratch.join("missing").exists());

    let systemd = systemd_layout(&scratch, "missing2/systemd/user", true);
    assert_eq!(systemd.status().unwrap(), Status::Absent);
    assert!(!scratch.join("missing2").exists());
}

#[test]
fn module_launches_no_external_tools() {
    for source in [
        include_str!("../src/autostart/mod.rs"),
        include_str!("../src/autostart/unix.rs"),
        include_str!("../src/autostart/windows.rs"),
        include_str!("../src/autostart/plist.rs"),
        include_str!("../src/autostart/unit.rs"),
        include_str!("../src/autostart/command_line.rs"),
    ] {
        assert!(!source.contains("Command::new"));
        assert!(!source.contains("std::process::Command"));
    }
}

#[test]
fn mechanisms_are_not_interchangeable() {
    let scratch = Scratch::new();
    let launch_on_systemd = Autostart::new(Layout::systemd_user(scratch.join("a"), true));
    assert_eq!(launch_on_systemd.mechanism(), Mechanism::SystemdUser);
    assert!(
        launch_on_systemd
            .install(Path::new("/opt/Totum/totum-daemon"))
            .is_ok()
    );
}
