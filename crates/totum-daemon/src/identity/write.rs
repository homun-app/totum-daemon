//! Creazione dei file iniziali di identità e registro.
//!
//! Ogni file viene scritto in un temporaneo `.identity.<pid>.<contatore>.tmp`
//! con nome esclusivo e modalità 0600 imposta dalla piattaforma, sincronizzato
//! e pubblicato con una rinomina che **non sostituisce** la destinazione. I due
//! file non formano una coppia atomica: la pubblicazione di ciascuno è il suo
//! punto di commit, e un arresto fra i due lascia uno stato iniziale parziale
//! che il riavvio completa.
//!
//! La pulizia rimuove soltanto i nomi del namespace `.identity.`; i temporanei
//! della configurazione e ogni altro oggetto restano intatti.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::IdentityError;
use crate::platform::{DirectorySync, PlatformError, VerifiedRoot, sync_file};

const TEMP_PREFIX: &str = ".identity.";
const TEMP_SUFFIX: &str = ".tmp";
const MAX_TEMP_ATTEMPTS: u32 = 16;

/// Esito della pubblicazione di un file iniziale.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Commit {
    /// Il file è stato pubblicato e la directory è stata sincronizzata.
    Committed,
    /// Il file è stato pubblicato, ma la voce di directory non è stata
    /// sincronizzata: la durabilità non è confermata. Non è un errore e non
    /// esiste un nuovo tentativo automatico.
    CommittedNotDurable { detail: String },
}

impl Commit {
    /// Indica se la durabilità è stata confermata dalla piattaforma.
    pub fn durable(&self) -> bool {
        matches!(self, Commit::Committed)
    }
}

/// File iniziale pubblicato, con l'esito della sua sincronizzazione.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Created {
    file: &'static str,
    commit: Commit,
}

impl Created {
    /// Nome del file pubblicato.
    pub fn file_name(&self) -> &'static str {
        self.file
    }

    /// Esito di commit e durabilità per quel file.
    pub fn commit(&self) -> &Commit {
        &self.commit
    }
}

/// Punti di guasto osservabili dalle prove; non esistono nel binario operativo.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Step {
    CreateTemp,
    WriteTemp,
    SyncTemp,
    Publish,
    SyncDirectory,
}

fn checkpoint(step: Step, action: &'static str, path: &Path) -> Result<(), IdentityError> {
    #[cfg(test)]
    if fault::crash_armed(step) {
        std::process::abort();
    }
    #[cfg(test)]
    if fault::armed(step) {
        fault::clear();
        return Err(IdentityError::write(
            action,
            PlatformError::io(path, std::io::Error::other("guasto simulato")),
        ));
    }
    let _ = (step, action, path);
    Ok(())
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn next_temp_name() -> OsString {
    let counter = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    OsString::from(format!(
        "{TEMP_PREFIX}{}.{counter}{TEMP_SUFFIX}",
        std::process::id()
    ))
}

/// Riconosce un nome del namespace riservato `.identity.<pid>.<n>.tmp`.
pub(super) fn is_reserved_temporary(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    let Some(middle) = bytes
        .strip_prefix(TEMP_PREFIX.as_bytes())
        .and_then(|rest| rest.strip_suffix(TEMP_SUFFIX.as_bytes()))
    else {
        return false;
    };
    let mut groups = middle.split(|byte| *byte == b'.');
    matches!(
        (groups.next(), groups.next(), groups.next()),
        (Some(process), Some(counter), None)
            if is_decimal(process) && is_decimal(counter)
    )
}

fn is_decimal(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(|byte| byte.is_ascii_digit())
}

pub(super) struct PendingFile<'a> {
    root: &'a VerifiedRoot,
    name: OsString,
    remove_on_drop: bool,
}

impl<'a> PendingFile<'a> {
    fn new(root: &'a VerifiedRoot, name: OsString) -> Self {
        Self {
            root,
            name,
            remove_on_drop: true,
        }
    }

    fn name(&self) -> &OsStr {
        &self.name
    }

    fn path(&self) -> PathBuf {
        self.root.path().join(&self.name)
    }

    fn keep(mut self) {
        self.remove_on_drop = false;
    }
}

impl Drop for PendingFile<'_> {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = self.root.remove_file(&self.name);
        }
    }
}

fn create_temporary<'a>(
    root: &'a VerifiedRoot,
    step: Step,
) -> Result<(PendingFile<'a>, File), IdentityError> {
    create_temporary_with(root, step, next_temp_name)
}

/// Come [`create_temporary`], con la sequenza dei nomi iniettabile dalle prove.
pub(super) fn create_temporary_with<'a, F>(
    root: &'a VerifiedRoot,
    step: Step,
    mut next_name: F,
) -> Result<(PendingFile<'a>, File), IdentityError>
where
    F: FnMut() -> OsString,
{
    checkpoint(step, "creazione del temporaneo", root.path())?;
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let name = next_name();
        match root.create_file(&name) {
            Ok(file) => return Ok((PendingFile::new(root, name), file)),
            Err(PlatformError::AlreadyExists { .. }) => continue,
            Err(error) => return Err(IdentityError::write("creazione del temporaneo", error)),
        }
    }
    Err(IdentityError::TemporaryCollision {
        attempts: MAX_TEMP_ATTEMPTS,
    })
}

fn fill_temporary(
    pending: &PendingFile<'_>,
    mut file: File,
    bytes: &[u8],
    write_step: Step,
    sync_step: Step,
) -> Result<(), IdentityError> {
    let path = pending.path();
    checkpoint(write_step, "scrittura del temporaneo", &path)?;
    file.write_all(bytes).map_err(|error| {
        IdentityError::write("scrittura del temporaneo", PlatformError::io(&path, error))
    })?;
    checkpoint(sync_step, "sincronizzazione del temporaneo", &path)?;
    sync_file(&file, &path)
        .map_err(|error| IdentityError::write("sincronizzazione del temporaneo", error))
}

/// Sincronizza la directory dopo la pubblicazione e traduce l'esito: da qui il
/// commit è già avvenuto e un errore non annulla la creazione.
fn finish(root: &VerifiedRoot) -> Commit {
    if let Err(error) = checkpoint(
        Step::SyncDirectory,
        "sincronizzazione della directory",
        root.path(),
    ) {
        return Commit::CommittedNotDurable {
            detail: error.to_string(),
        };
    }
    match root.sync_directory() {
        Ok(DirectorySync::Synced) => Commit::Committed,
        Ok(DirectorySync::Unsupported) => Commit::CommittedNotDurable {
            detail: "la piattaforma non sincronizza la voce di directory".to_owned(),
        },
        Err(error) => Commit::CommittedNotDurable {
            detail: error.to_string(),
        },
    }
}

/// Crea `name` con i byte indicati soltanto se la destinazione non esiste.
pub(super) fn create_missing(
    root: &VerifiedRoot,
    name: &'static str,
    bytes: &[u8],
) -> Result<Created, IdentityError> {
    create_missing_with(root, name, bytes, || {})
}

/// Come [`create_missing`], con un aggancio di prova eseguito subito prima
/// della pubblicazione. L'aggancio permette di far comparire la destinazione
/// fra la classificazione e la rinomina senza modificare il binario.
pub(super) fn create_missing_with<F>(
    root: &VerifiedRoot,
    name: &'static str,
    bytes: &[u8],
    before_publish: F,
) -> Result<Created, IdentityError>
where
    F: FnOnce(),
{
    root.verify_path().map_err(IdentityError::Protection)?;
    let (pending, file) = create_temporary(root, Step::CreateTemp)?;
    fill_temporary(&pending, file, bytes, Step::WriteTemp, Step::SyncTemp)?;

    before_publish();
    checkpoint(
        Step::Publish,
        "pubblicazione del file iniziale",
        &root.path().join(name),
    )?;
    match root.publish_file(pending.name(), OsStr::new(name)) {
        Ok(()) => {}
        Err(PlatformError::AlreadyExists { path }) => {
            return Err(IdentityError::AppearedDuringInitialization { path });
        }
        Err(error) => {
            return Err(IdentityError::write(
                "pubblicazione del file iniziale",
                error,
            ));
        }
    }
    pending.keep();

    Ok(Created {
        file: name,
        commit: finish(root),
    })
}

/// Rimuove i temporanei del namespace riservato al modulo.
///
/// Il nome non identifica il processo: con la precondizione di un solo
/// scrittore, garantita dal lock esclusivo e dall'accesso `&mut LockedRoot`
/// richiesto da `initialize`, ogni nome conforme è un residuo del modulo. I nomi
/// fuori dal namespace non vengono toccati.
pub(super) fn discard_temporaries(root: &VerifiedRoot) -> Result<Vec<OsString>, IdentityError> {
    let names = root
        .list_file_names()
        .map_err(|error| IdentityError::write("lettura della radice", error))?;
    let mut removed = Vec::new();
    for name in names {
        if !is_reserved_temporary(&name) {
            continue;
        }
        match root.remove_file(&name) {
            Ok(()) => removed.push(name),
            Err(PlatformError::FileAbsent { .. }) => {}
            Err(error) => return Err(IdentityError::write("rimozione del temporaneo", error)),
        }
    }
    removed.sort();
    Ok(removed)
}

#[cfg(test)]
pub(super) mod fault {
    //! Guasti simulati per thread, attivi soltanto nelle prove della libreria.

    use std::cell::Cell;

    use super::Step;

    thread_local! {
        static ARMED: Cell<Option<Step>> = const { Cell::new(None) };
        static CRASH: Cell<Option<Step>> = const { Cell::new(None) };
    }

    pub(in crate::identity) fn arm(step: Step) {
        ARMED.with(|armed| armed.set(Some(step)));
    }

    pub(in crate::identity) fn clear() {
        ARMED.with(|armed| armed.set(None));
    }

    pub(super) fn armed(step: Step) -> bool {
        ARMED.with(|armed| armed.get() == Some(step))
    }

    pub(in crate::identity) fn arm_crash(step: Step) {
        CRASH.with(|armed| armed.set(Some(step)));
    }

    pub(super) fn crash_armed(step: Step) -> bool {
        CRASH.with(|armed| armed.get() == Some(step))
    }
}
