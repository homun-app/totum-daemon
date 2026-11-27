//! Implementazione Unix dell'avvio automatico: Launch Agent e systemd utente.
//!
//! Le due varianti condividono la classificazione degli oggetti e le scritture
//! protette; cambiano soltanto formato, nomi e presenza del collegamento. La
//! directory standard non viene mai creata con una modalità diversa da 0700,
//! non viene mai riparata e non viene mai seguita se è un collegamento.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::process::geteuid;

use crate::platform::{self, DirectorySync, EntryKind, PlatformError, VerifiedRoot};

use super::{
    AutostartError, Durability, InstallChange, InstallFailure, InstallReport, Layout, Mechanism,
    RemoveChange, RemoveFailure, RemoveReport, Status,
};

const MAX_OBJECT_BYTES: u64 = 64 * 1024;
const TEMP_PREFIX: &str = ".autostart.";
const TEMP_ATTEMPTS: u32 = 16;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Stato classificato di un file di registrazione.
enum Object {
    Absent,
    Owned { text: String, program: PathBuf },
    Incompatible { detail: String },
}

/// Stato classificato del collegamento systemd.
enum Link {
    Absent,
    Owned,
    Incompatible { detail: String },
}

pub(super) fn status(layout: &Layout) -> Result<Status, AutostartError> {
    match layout.mechanism() {
        Mechanism::LaunchAgent => status_launch_agent(layout.directory()),
        Mechanism::SystemdUser => status_systemd(layout),
        Mechanism::RunKey => Ok(Status::Unavailable {
            detail: "il valore Run è disponibile soltanto su Windows".to_owned(),
        }),
    }
}

pub(super) fn install(layout: &Layout, binary: &Path) -> Result<InstallReport, InstallFailure> {
    let mut report = InstallReport {
        program: binary.to_path_buf(),
        entry: InstallChange::NotAttempted,
        link: (layout.mechanism() == Mechanism::SystemdUser).then_some(InstallChange::NotAttempted),
        durability: Durability::Unknown,
    };
    let result = match layout.mechanism() {
        Mechanism::LaunchAgent => install_launch_agent(layout, binary, &mut report),
        Mechanism::SystemdUser => install_systemd(layout, binary, &mut report),
        Mechanism::RunKey => Err(AutostartError::Unavailable {
            detail: "il valore Run è disponibile soltanto su Windows".to_owned(),
        }),
    };
    match result {
        Ok(()) => Ok(report),
        Err(error) => Err(InstallFailure {
            report,
            error: Box::new(error),
        }),
    }
}

pub(super) fn remove(layout: &Layout) -> Result<RemoveReport, RemoveFailure> {
    let mut report = RemoveReport {
        entry: RemoveChange::NotAttempted,
        link: (layout.mechanism() == Mechanism::SystemdUser).then_some(RemoveChange::NotAttempted),
        durability: Durability::Unknown,
    };
    let result = match layout.mechanism() {
        Mechanism::LaunchAgent => remove_launch_agent(layout, &mut report),
        Mechanism::SystemdUser => remove_systemd(layout, &mut report),
        Mechanism::RunKey => Err(AutostartError::Unavailable {
            detail: "il valore Run è disponibile soltanto su Windows".to_owned(),
        }),
    };
    match result {
        Ok(()) => Ok(report),
        Err(error) => Err(RemoveFailure {
            report,
            error: Box::new(error),
        }),
    }
}

fn status_launch_agent(directory: &Path) -> Result<Status, AutostartError> {
    let root = match platform::open_containing_directory(directory) {
        Ok(root) => root,
        Err(error) => return open_error_status(error),
    };
    let Some(root) = root else {
        return Ok(Status::Absent);
    };
    match classify_file(&root, super::plist::FILE_NAME, super::plist::parse_owned)? {
        Object::Absent => Ok(Status::Absent),
        Object::Owned { program, .. } => Ok(Status::Registered { program }),
        Object::Incompatible { detail } => Ok(Status::Incompatible { detail }),
    }
}

fn status_systemd(layout: &Layout) -> Result<Status, AutostartError> {
    if !layout.systemd_available() {
        return Ok(Status::Unavailable {
            detail: "systemd non rilevato: /run/systemd/system non esiste".to_owned(),
        });
    }
    let root = match platform::open_containing_directory(layout.directory()) {
        Ok(root) => root,
        Err(error) => return open_error_status(error),
    };
    let Some(root) = root else {
        return Ok(Status::Absent);
    };

    let unit = classify_file(&root, super::unit::FILE_NAME, super::unit::parse_owned)?;
    let wants = match root.open_subdirectory(OsStr::new(super::unit::WANTS_DIR)) {
        Ok(wants) => wants,
        Err(error) => return open_error_status(error),
    };
    let link = match &wants {
        Some(wants) => classify_link(wants, &unit_path(layout.directory()))?,
        None => Link::Absent,
    };

    match (unit, link) {
        (Object::Owned { program, .. }, Link::Owned) => Ok(Status::Registered { program }),
        (Object::Absent, Link::Absent) => Ok(Status::Absent),
        (Object::Absent, Link::Owned) => Ok(Status::Incompatible {
            detail: "collegamento di Totum presente senza unità".to_owned(),
        }),
        (Object::Owned { .. }, Link::Absent) => Ok(Status::Incompatible {
            detail: "unità di Totum presente senza collegamento in default.target.wants".to_owned(),
        }),
        (Object::Incompatible { detail }, _) | (_, Link::Incompatible { detail }) => {
            Ok(Status::Incompatible { detail })
        }
    }
}

fn install_launch_agent(
    layout: &Layout,
    binary: &Path,
    report: &mut InstallReport,
) -> Result<(), AutostartError> {
    refuse_elevated()?;
    let document = super::plist::document(binary)?;
    let root = prepare_directory(layout.directory())?;
    let path = root.path().join(super::plist::FILE_NAME);

    let existing = classify_file(&root, super::plist::FILE_NAME, super::plist::parse_owned)?;
    report.entry = match existing {
        Object::Absent => {
            write_new(&root, super::plist::FILE_NAME, &document)?;
            InstallChange::Created
        }
        Object::Incompatible { detail } => {
            return Err(AutostartError::IncompatibleObject { path, detail });
        }
        Object::Owned { text, .. } if text == document => InstallChange::Unchanged,
        Object::Owned { text, .. } => replace_owned(
            &root,
            super::plist::FILE_NAME,
            &text,
            &document,
            super::plist::parse_owned,
        )?,
    };
    if report.entry != InstallChange::Unchanged {
        report.durability = sync_one(&root);
    }
    Ok(())
}

fn install_systemd(
    layout: &Layout,
    binary: &Path,
    report: &mut InstallReport,
) -> Result<(), AutostartError> {
    refuse_elevated()?;
    if !layout.systemd_available() {
        return Err(AutostartError::Unavailable {
            detail: "systemd non rilevato: /run/systemd/system non esiste".to_owned(),
        });
    }
    let document = super::unit::document(binary)?;
    let unit_path = unit_path(layout.directory());

    // Classificazione completa prima di qualunque modifica.
    let existing_root = open_or_absent(layout.directory())?;
    let mut unit = Object::Absent;
    let mut link = Link::Absent;
    if let Some(root) = &existing_root {
        unit = classify_file(root, super::unit::FILE_NAME, super::unit::parse_owned)?;
        if let Object::Incompatible { detail } = &unit {
            return Err(AutostartError::IncompatibleObject {
                path: unit_path,
                detail: detail.clone(),
            });
        }
        if let Some(wants) = root.open_subdirectory(OsStr::new(super::unit::WANTS_DIR))? {
            link = classify_link(&wants, &unit_path)?;
            if let Link::Incompatible { detail } = &link {
                return Err(AutostartError::IncompatibleObject {
                    path: wants.path().join(super::unit::FILE_NAME),
                    detail: detail.clone(),
                });
            }
        }
    }

    let root = match existing_root {
        Some(root) => root,
        None => prepare_directory(layout.directory())?,
    };

    report.entry = match unit {
        Object::Absent => {
            write_new(&root, super::unit::FILE_NAME, &document)?;
            InstallChange::Created
        }
        Object::Owned { text, .. } if text == document => InstallChange::Unchanged,
        Object::Owned { text, .. } => replace_owned(
            &root,
            super::unit::FILE_NAME,
            &text,
            &document,
            super::unit::parse_owned,
        )?,
        Object::Incompatible { .. } => unreachable!("classificato prima delle modifiche"),
    };

    let wants = match root.open_subdirectory(OsStr::new(super::unit::WANTS_DIR))? {
        Some(wants) => wants,
        None => root.ensure_subdirectory(OsStr::new(super::unit::WANTS_DIR))?,
    };
    report.link = Some(match link {
        Link::Owned => InstallChange::Unchanged,
        Link::Absent => create_link(&wants, &unit_path)?,
        Link::Incompatible { .. } => unreachable!("classificato prima delle modifiche"),
    });

    if report.entry != InstallChange::Unchanged || report.link != Some(InstallChange::Unchanged) {
        report.durability = sync_both(&root, &wants);
    }
    Ok(())
}

fn remove_launch_agent(layout: &Layout, report: &mut RemoveReport) -> Result<(), AutostartError> {
    refuse_elevated()?;
    let root = match open_or_absent(layout.directory())? {
        Some(root) => root,
        None => {
            report.entry = RemoveChange::AlreadyAbsent;
            return Ok(());
        }
    };
    let path = root.path().join(super::plist::FILE_NAME);
    match classify_file(&root, super::plist::FILE_NAME, super::plist::parse_owned)? {
        Object::Absent => {
            report.entry = RemoveChange::AlreadyAbsent;
        }
        Object::Incompatible { detail } => {
            return Err(AutostartError::IncompatibleObject { path, detail });
        }
        Object::Owned { .. } => {
            root.remove_file(OsStr::new(super::plist::FILE_NAME))?;
            report.entry = RemoveChange::Removed;
            report.durability = sync_one(&root);
        }
    }
    Ok(())
}

fn remove_systemd(layout: &Layout, report: &mut RemoveReport) -> Result<(), AutostartError> {
    refuse_elevated()?;
    let root = match open_or_absent(layout.directory())? {
        Some(root) => root,
        None => {
            report.entry = RemoveChange::AlreadyAbsent;
            report.link = Some(RemoveChange::AlreadyAbsent);
            return Ok(());
        }
    };
    let unit_path = unit_path(layout.directory());
    let unit = classify_file(&root, super::unit::FILE_NAME, super::unit::parse_owned)?;
    let wants = root.open_subdirectory(OsStr::new(super::unit::WANTS_DIR))?;
    let link = match &wants {
        Some(wants) => classify_link(wants, &unit_path)?,
        None => Link::Absent,
    };

    if let Object::Incompatible { detail } = &unit {
        return Err(AutostartError::IncompatibleObject {
            path: unit_path,
            detail: detail.clone(),
        });
    }
    if let Some(wants) = &wants
        && let Link::Incompatible { detail } = &link
    {
        return Err(AutostartError::IncompatibleObject {
            path: wants.path().join(super::unit::FILE_NAME),
            detail: detail.clone(),
        });
    }

    let mut wrote = false;
    report.link = Some(match (link, &wants) {
        (Link::Owned, Some(wants)) => {
            wants.remove_symlink(OsStr::new(super::unit::FILE_NAME))?;
            wrote = true;
            RemoveChange::Removed
        }
        (Link::Owned, None) => RemoveChange::AlreadyAbsent,
        (Link::Absent | Link::Incompatible { .. }, _) => RemoveChange::AlreadyAbsent,
    });
    report.entry = match unit {
        Object::Owned { .. } => {
            root.remove_file(OsStr::new(super::unit::FILE_NAME))?;
            wrote = true;
            RemoveChange::Removed
        }
        Object::Absent | Object::Incompatible { .. } => RemoveChange::AlreadyAbsent,
    };

    if wrote {
        report.durability = match &wants {
            Some(wants) => sync_both(&root, wants),
            None => sync_one(&root),
        };
    }
    Ok(())
}

fn classify_file(
    root: &VerifiedRoot,
    name: &'static str,
    parse: fn(&str) -> Option<PathBuf>,
) -> Result<Object, AutostartError> {
    let path = root.path().join(name);
    let Some(info) = root.entry_info(OsStr::new(name))? else {
        return Ok(Object::Absent);
    };
    if info.kind != EntryKind::Regular {
        return Ok(Object::Incompatible {
            detail: format!("atteso un file regolare, trovato {:?}", info.kind),
        });
    }
    if info.uid != euid() {
        return Ok(Object::Incompatible {
            detail: format!(
                "proprietario errato: atteso {}, trovato {}",
                euid(),
                info.uid
            ),
        });
    }
    if info.mode != 0o600 {
        return Ok(Object::Incompatible {
            detail: format!("modalità errata: attesa 0600, trovata {:04o}", info.mode),
        });
    }

    let file = match root.open_file(OsStr::new(name)) {
        Ok(file) => file,
        Err(PlatformError::FileAbsent { .. }) => return Ok(Object::Absent),
        Err(error) => match super::protection_detail(&error) {
            Some(detail) => return Ok(Object::Incompatible { detail }),
            None => return Err(error.into()),
        },
    };
    let mut bytes = Vec::new();
    file.take(MAX_OBJECT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| AutostartError::Io {
            path: path.clone(),
            source,
        })?;
    if bytes.len() as u64 > MAX_OBJECT_BYTES {
        return Ok(Object::Incompatible {
            detail: format!("oggetto più grande di {MAX_OBJECT_BYTES} byte"),
        });
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            return Ok(Object::Incompatible {
                detail: "contenuto non UTF-8".to_owned(),
            });
        }
    };
    match parse(&text) {
        Some(program) => Ok(Object::Owned { text, program }),
        None => Ok(Object::Incompatible {
            detail: "contenuto non riconosciuto come registrazione di Totum".to_owned(),
        }),
    }
}

fn classify_link(wants: &VerifiedRoot, unit_path: &Path) -> Result<Link, AutostartError> {
    let Some(info) = wants.entry_info(OsStr::new(super::unit::FILE_NAME))? else {
        return Ok(Link::Absent);
    };
    if info.kind != EntryKind::Symlink {
        return Ok(Link::Incompatible {
            detail: format!("atteso un collegamento simbolico, trovato {:?}", info.kind),
        });
    }
    if info.uid != euid() {
        return Ok(Link::Incompatible {
            detail: format!(
                "proprietario del collegamento errato: atteso {}, trovato {}",
                euid(),
                info.uid
            ),
        });
    }
    match wants.read_symlink_target(OsStr::new(super::unit::FILE_NAME)) {
        Ok(Some(target)) if super::unit::is_owned_link_target(&target, unit_path) => {
            Ok(Link::Owned)
        }
        Ok(Some(target)) => Ok(Link::Incompatible {
            detail: format!(
                "il collegamento punta a una destinazione non gestita: {}",
                target.display()
            ),
        }),
        Ok(None) => Ok(Link::Absent),
        Err(PlatformError::FileAbsent { .. }) => Ok(Link::Absent),
        Err(error) => match super::protection_detail(&error) {
            Some(detail) => Ok(Link::Incompatible { detail }),
            None => Err(error.into()),
        },
    }
}

fn prepare_directory(directory: &Path) -> Result<VerifiedRoot, AutostartError> {
    match platform::prepare_containing_directory(directory) {
        Ok(root) => Ok(root),
        Err(error) => match super::protection_detail(&error) {
            Some(detail) => Err(AutostartError::UnsafeObject {
                path: directory.to_path_buf(),
                detail,
            }),
            None => Err(error.into()),
        },
    }
}

fn open_or_absent(directory: &Path) -> Result<Option<VerifiedRoot>, AutostartError> {
    match platform::open_containing_directory(directory) {
        Ok(root) => Ok(root),
        Err(error) => match super::protection_detail(&error) {
            Some(detail) => Err(AutostartError::UnsafeObject {
                path: directory.to_path_buf(),
                detail,
            }),
            None => Err(error.into()),
        },
    }
}

fn open_error_status(error: PlatformError) -> Result<Status, AutostartError> {
    match super::protection_detail(&error) {
        Some(detail) => Ok(Status::Incompatible { detail }),
        None => Err(error.into()),
    }
}

fn refuse_elevated() -> Result<(), AutostartError> {
    if platform::is_elevated()? {
        return Err(AutostartError::ElevatedExecution);
    }
    Ok(())
}

fn write_new(root: &VerifiedRoot, name: &'static str, text: &str) -> Result<(), AutostartError> {
    let (pending, mut file) = create_temporary(root)?;
    let path = pending.path();
    file.write_all(text.as_bytes())
        .map_err(|source| AutostartError::Io {
            path: path.clone(),
            source,
        })?;
    platform::sync_file(&file, &path)?;
    match root.publish_file(pending.name(), OsStr::new(name)) {
        Ok(()) => {
            pending.keep();
            Ok(())
        }
        Err(PlatformError::AlreadyExists { path }) => {
            Err(AutostartError::ChangedDuringRegistration { path })
        }
        Err(error) => Err(error.into()),
    }
}

fn replace_owned(
    root: &VerifiedRoot,
    name: &'static str,
    old_text: &str,
    new_text: &str,
    parse: fn(&str) -> Option<PathBuf>,
) -> Result<InstallChange, AutostartError> {
    let path = root.path().join(name);
    match classify_file(root, name, parse)? {
        Object::Absent => {
            write_new(root, name, new_text)?;
            return Ok(InstallChange::Created);
        }
        Object::Owned { text, .. } if text == new_text => return Ok(InstallChange::Unchanged),
        Object::Owned { text, .. } if text == old_text => {}
        _ => {
            return Err(AutostartError::ChangedDuringRegistration { path });
        }
    }

    let (pending, mut file) = create_temporary(root)?;
    let temp_path = pending.path();
    file.write_all(new_text.as_bytes())
        .map_err(|source| AutostartError::Io {
            path: temp_path.clone(),
            source,
        })?;
    platform::sync_file(&file, &temp_path)?;
    root.replace_file(pending.name(), OsStr::new(name))?;
    pending.keep();
    Ok(InstallChange::Updated)
}

fn create_link(wants: &VerifiedRoot, unit_path: &Path) -> Result<InstallChange, AutostartError> {
    match wants.create_symlink(
        OsStr::new(super::unit::FILE_NAME),
        OsStr::new(super::unit::LINK_TARGET),
    ) {
        Ok(()) => Ok(InstallChange::Created),
        Err(PlatformError::AlreadyExists { path }) => match classify_link(wants, unit_path)? {
            Link::Owned => Ok(InstallChange::Unchanged),
            Link::Absent => Err(AutostartError::ChangedDuringRegistration { path }),
            Link::Incompatible { detail } => {
                Err(AutostartError::IncompatibleObject { path, detail })
            }
        },
        Err(error) => Err(error.into()),
    }
}

struct PendingFile<'a> {
    root: &'a VerifiedRoot,
    name: OsString,
    keep: bool,
}

impl PendingFile<'_> {
    fn name(&self) -> &OsStr {
        &self.name
    }

    fn path(&self) -> PathBuf {
        self.root.path().join(&self.name)
    }

    fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for PendingFile<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = self.root.remove_file(&self.name);
        }
    }
}

fn create_temporary(root: &VerifiedRoot) -> Result<(PendingFile<'_>, File), AutostartError> {
    for _ in 0..TEMP_ATTEMPTS {
        let counter = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let name = OsString::from(format!("{TEMP_PREFIX}{}.{counter}.tmp", std::process::id()));
        match root.create_file(&name) {
            Ok(file) => {
                return Ok((
                    PendingFile {
                        root,
                        name,
                        keep: false,
                    },
                    file,
                ));
            }
            Err(PlatformError::AlreadyExists { .. }) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(AutostartError::Io {
        path: root.path().to_path_buf(),
        source: std::io::Error::other(format!(
            "nessun nome temporaneo disponibile dopo {TEMP_ATTEMPTS} tentativi"
        )),
    })
}

fn sync_one(root: &VerifiedRoot) -> Durability {
    match root.sync_directory() {
        Ok(DirectorySync::Synced) => Durability::Confirmed,
        Ok(DirectorySync::Unsupported) => Durability::NotConfirmed {
            detail: "la piattaforma non sincronizza la voce di directory".to_owned(),
        },
        Err(error) => Durability::NotConfirmed {
            detail: error.to_string(),
        },
    }
}

fn sync_both(first: &VerifiedRoot, second: &VerifiedRoot) -> Durability {
    let first_result = sync_outcome(first);
    let second_result = sync_outcome(second);
    match (first_result, second_result) {
        (Ok(()), Ok(())) => Durability::Confirmed,
        (first, second) => {
            let mut details = Vec::new();
            if let Err(detail) = first {
                details.push(detail);
            }
            if let Err(detail) = second {
                details.push(detail);
            }
            Durability::NotConfirmed {
                detail: details.join("; "),
            }
        }
    }
}

fn sync_outcome(root: &VerifiedRoot) -> Result<(), String> {
    match root.sync_directory() {
        Ok(DirectorySync::Synced) => Ok(()),
        Ok(DirectorySync::Unsupported) => {
            Err("la piattaforma non sincronizza la voce di directory".to_owned())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn unit_path(directory: &Path) -> PathBuf {
    directory.join(super::unit::FILE_NAME)
}

fn euid() -> u32 {
    geteuid().as_raw()
}
