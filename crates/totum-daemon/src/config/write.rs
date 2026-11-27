//! Scrittura della configurazione: temporanei, backup, sostituzione e recupero.
//!
//! Ogni generazione viene scritta in un temporaneo `0600` nella radice,
//! sincronizzata e pubblicata con una rinomina atomica. Il backup della
//! generazione corrente viene preparato in un secondo temporaneo e rinominato,
//! senza spostare il principale: dopo la prima creazione `config.json` non è
//! mai assente. Prima di creare un temporaneo, un salvataggio rivalida lo stato
//! su disco con la stessa classificazione dell'apertura; il recupero resta un
//! percorso separato e non viene mai attivato da un salvataggio.
//!
//! I nomi `.config.<pid>.<contatore>.tmp` formano il namespace riservato ai
//! temporanei del modulo. La pulizia rimuove ogni nome del namespace,
//! indipendentemente dal PID: vale la precondizione di un solo scrittore, per
//! cui non esistono temporanei attivi di altri processi. Ogni oggetto fuori dal
//! namespace resta intatto.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::error::ConfigError;
use super::{
    CONFIG_BACKUP_FILE, CONFIG_FILE, ConfigV1, Decision, GenerationFailure, MigrationSource,
    document, read_bounded,
};
use crate::platform::{DirectorySync, PlatformError, VerifiedRoot, sync_file};

const TEMP_PREFIX: &str = ".config.";
const TEMP_SUFFIX: &str = ".tmp";
const CORRUPT_PREFIX: &str = "config.corrupt.";
const MAX_TEMP_ATTEMPTS: u32 = 16;
const MAX_PRESERVE_ATTEMPTS: u32 = 16;

/// Esito di un'operazione che ha raggiunto o superato il proprio punto di
/// commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Commit {
    /// Commit effettuato; la durabilità dichiarata è quella della piattaforma.
    Committed { revision: u64 },
    /// Commit effettuato, ma la durabilità non è confermata: la voce di
    /// directory non è stata sincronizzata. Il salvataggio non è fallito e non
    /// va ritentato automaticamente.
    CommittedNotDurable { revision: u64, detail: String },
}

impl Commit {
    /// Revisione della generazione resa visibile.
    pub fn revision(&self) -> u64 {
        match self {
            Commit::Committed { revision } | Commit::CommittedNotDurable { revision, .. } => {
                *revision
            }
        }
    }

    /// Indica se la durabilità è stata confermata dalla piattaforma.
    pub fn durable(&self) -> bool {
        matches!(self, Commit::Committed { .. })
    }
}

/// Punti di guasto osservabili dalle prove; non esistono nel binario operativo.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Step {
    CreateTemp,
    WriteTemp,
    SyncTemp,
    CreateBackupTemp,
    WriteBackupTemp,
    SyncBackupTemp,
    ReplaceBackup,
    PublishMain,
    SyncDirectory,
    PreserveCorrupt,
}

/// Fallisce il passo indicato quando la prova lo ha armato, senza toccare il
/// file system; negli altri casi non fa nulla.
///
/// Se la prova ha armato un arresto, il processo termina senza unwinding:
/// nessun `Drop` viene eseguito e i temporanei restano dove sono. È un
/// meccanismo esclusivamente di test, assente dai binari operativi.
fn checkpoint(step: Step, action: &'static str, path: &Path) -> Result<(), ConfigError> {
    #[cfg(test)]
    if fault::crash_armed(step) {
        std::process::abort();
    }
    #[cfg(test)]
    if fault::armed(step) {
        fault::clear();
        return Err(ConfigError::write(
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

/// Riconosce un nome del namespace dei temporanei riservato al modulo:
/// `.config.<pid>.<n>.tmp`.
///
/// Il nome non identifica il processo che lo ha scritto: il PID è un
/// componente del nome, non un criterio di proprietà. Con la precondizione di
/// un solo scrittore, la pulizia considera ogni nome del namespace un proprio
/// temporaneo residuo. Un file con un altro nome, comprese le copie
/// `config.corrupt.*`, non corrisponde e non viene mai rimosso.
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

/// File creato ma non ancora pubblicato: se non viene disarmato, il `Drop` lo
/// rimuove. La rimozione è tentata una volta e un eventuale residuo viene
/// scartato dalla pulizia della successiva apertura.
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

    fn keep(mut self) -> OsString {
        self.remove_on_drop = false;
        self.name.clone()
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
) -> Result<(PendingFile<'a>, File), ConfigError> {
    create_temporary_with(root, step, next_temp_name)
}

/// Come [`create_temporary`], con la sequenza dei nomi iniettabile dalle prove.
pub(super) fn create_temporary_with<'a, F>(
    root: &'a VerifiedRoot,
    step: Step,
    mut next_name: F,
) -> Result<(PendingFile<'a>, File), ConfigError>
where
    F: FnMut() -> OsString,
{
    checkpoint(step, "creazione del temporaneo", root.path())?;
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let name = next_name();
        match root.create_file(&name) {
            Ok(file) => return Ok((PendingFile::new(root, name), file)),
            Err(PlatformError::AlreadyExists { .. }) => continue,
            Err(error) => return Err(ConfigError::write("creazione del temporaneo", error)),
        }
    }
    Err(ConfigError::TemporaryCollision {
        attempts: MAX_TEMP_ATTEMPTS,
    })
}

fn fill_temporary(
    pending: &PendingFile<'_>,
    mut file: File,
    bytes: &[u8],
    write_step: Step,
    sync_step: Step,
) -> Result<(), ConfigError> {
    let path = pending.path();
    checkpoint(write_step, "scrittura del temporaneo", &path)?;
    file.write_all(bytes).map_err(|error| {
        ConfigError::write("scrittura del temporaneo", PlatformError::io(&path, error))
    })?;
    checkpoint(sync_step, "sincronizzazione del temporaneo", &path)?;
    sync_file(&file, &path)
        .map_err(|error| ConfigError::write("sincronizzazione del temporaneo", error))
}

/// Sincronizza la directory dopo la rinomina e traduce l'esito: da qui il
/// commit è già avvenuto e un errore non annulla il salvataggio.
fn finish(root: &VerifiedRoot, revision: u64) -> Commit {
    let not_durable = |detail: String| Commit::CommittedNotDurable { revision, detail };
    if let Err(error) = checkpoint(
        Step::SyncDirectory,
        "sincronizzazione della directory",
        root.path(),
    ) {
        return not_durable(error.to_string());
    }
    match root.sync_directory() {
        Ok(DirectorySync::Synced) => Commit::Committed { revision },
        Ok(DirectorySync::Unsupported) => {
            not_durable("la piattaforma non sincronizza la voce di directory".to_owned())
        }
        Err(error) => not_durable(error.to_string()),
    }
}

/// Prima creazione: pubblica la revisione 0 senza sostituire nulla.
///
/// La pubblicazione fallisce con [`ConfigError::AppearedDuringFirstRun`] se
/// `config.json` esiste: il chiamante torna alla classificazione e nessun
/// oggetto comparente viene sovrascritto.
pub(super) fn create_initial(root: &VerifiedRoot, config: ConfigV1) -> Result<Commit, ConfigError> {
    root.verify_path().map_err(ConfigError::Protection)?;
    let bytes = document::serialize(config.revision())?;

    let (pending, file) = create_temporary(root, Step::CreateTemp)?;
    fill_temporary(&pending, file, &bytes, Step::WriteTemp, Step::SyncTemp)?;

    checkpoint(
        Step::PublishMain,
        "pubblicazione del principale",
        &root.path().join(CONFIG_FILE),
    )?;
    match root.publish_file(pending.name(), OsStr::new(CONFIG_FILE)) {
        Ok(()) => {}
        Err(PlatformError::AlreadyExists { path }) => {
            return Err(ConfigError::AppearedDuringFirstRun { path });
        }
        Err(error) => return Err(ConfigError::write("pubblicazione del principale", error)),
    }
    pending.keep();

    Ok(finish(root, config.revision()))
}

/// Rivalida i due file su disco prima di toccare il file system.
///
/// Usa la stessa precedenza della classificazione, limitata ai due file:
/// schema futuro, protezioni o tipo errati, revisione non valida o in
/// diminuzione e principale assente o corrotto producono lo stesso rifiuto,
/// senza scrivere. Se lo stato è normale ma la revisione del principale non è
/// quella attesa dallo [`Store`](super::Store), il salvataggio si ferma con
/// [`ConfigError::RevisionDivergence`]: la generazione divergente non viene né
/// adottata né sovrascritta. Un principale da recuperare produce
/// [`ConfigError::RecoveryRequired`]: il recupero resta un percorso esplicito e
/// non viene applicato da un salvataggio.
///
/// Identità e registro non vengono riletti: la loro classificazione appartiene
/// a P7. Lo `Store` detiene il lock esclusivo della radice, quindi questa
/// rivalidazione è una difesa in profondità: rileva modifiche avvenute per vie
/// che non rispettano il lock, non solo da una seconda istanza cooperante.
fn revalidate(root: &VerifiedRoot, expected: u64) -> Result<(), ConfigError> {
    match super::classify::classify_generations(root) {
        Decision::Normal { config, .. } if config.revision() == expected => Ok(()),
        Decision::Normal { config, .. } => Err(ConfigError::RevisionDivergence {
            expected,
            found: config.revision(),
        }),
        Decision::Refused(error) => Err(error),
        Decision::MigrationRequired {
            from_schema_version,
            source,
            ..
        } => Err(ConfigError::MigrationRequired {
            from_schema_version,
            source,
        }),
        Decision::RecoveryRequired { main, .. } => Err(ConfigError::RecoveryRequired { main }),
        Decision::FirstRun { .. } => Err(ConfigError::NoValidGeneration {
            main: GenerationFailure::Absent,
            backup: GenerationFailure::Absent,
        }),
    }
}

/// Salvataggio successivo: dal principale `N` a `N+1`, con il backup della
/// generazione corrente preparato senza spostare il principale.
///
/// La generazione su disco viene rivalidata prima di creare temporanei: un
/// salvataggio non adotta né sovrascrive uno stato che l'apertura avrebbe
/// rifiutato, compresa una revisione diversa da quella attesa.
pub(super) fn save(root: &VerifiedRoot, current: ConfigV1) -> Result<Commit, ConfigError> {
    let revision = current
        .revision()
        .checked_add(1)
        .ok_or(ConfigError::RevisionOverflow {
            revision: current.revision(),
        })?;
    let bytes = document::serialize(revision)?;
    revalidate(root, current.revision())?;

    // I byte del backup sono quelli del principale caricato, riletto dopo la
    // rivalidazione con la stessa verifica della classificazione.
    let previous = read_generation(root, CONFIG_FILE)?;

    let (pending, file) = create_temporary(root, Step::CreateTemp)?;
    fill_temporary(&pending, file, &bytes, Step::WriteTemp, Step::SyncTemp)?;

    let (backup, backup_file) = create_temporary(root, Step::CreateBackupTemp)?;
    fill_temporary(
        &backup,
        backup_file,
        &previous,
        Step::WriteBackupTemp,
        Step::SyncBackupTemp,
    )?;
    checkpoint(
        Step::ReplaceBackup,
        "sostituzione del backup",
        &root.path().join(CONFIG_BACKUP_FILE),
    )?;
    root.replace_file(backup.name(), OsStr::new(CONFIG_BACKUP_FILE))
        .map_err(|error| ConfigError::write("sostituzione del backup", error))?;
    backup.keep();

    checkpoint(
        Step::PublishMain,
        "sostituzione del principale",
        &root.path().join(CONFIG_FILE),
    )?;
    root.replace_file(pending.name(), OsStr::new(CONFIG_FILE))
        .map_err(|error| ConfigError::write("sostituzione del principale", error))?;
    pending.keep();

    Ok(finish(root, revision))
}

/// Migrazione persistita di una generazione con schema precedente verso la v1.
///
/// Ripete l'ordine di un salvataggio: prima il temporaneo della nuova
/// generazione, poi il temporaneo del backup con i **byte originali** della
/// sorgente, quindi la sostituzione del backup e infine quella del principale,
/// che è il punto di commit. Se la sorgente è il backup, il principale
/// corrotto viene conservato in `config.corrupt.*` con la stessa regola del
/// recupero. La sorgente non viene mai sovrascritta prima che la nuova
/// generazione sia stata scritta e sincronizzata, e il backup conserva lo
/// schema precedente per il recupero.
pub(super) fn migrate(
    root: &VerifiedRoot,
    source: MigrationSource,
    original: &[u8],
    next: ConfigV1,
    main_failure: Option<&GenerationFailure>,
) -> Result<(Commit, Option<OsString>), ConfigError> {
    root.verify_path().map_err(ConfigError::Protection)?;
    let bytes = document::serialize(next.revision())?;

    let preserved = match (source, main_failure) {
        (MigrationSource::Backup, Some(GenerationFailure::Corrupt { .. })) => {
            Some(preserve_corrupt(root)?)
        }
        _ => None,
    };

    let (pending, file) = create_temporary(root, Step::CreateTemp)?;
    fill_temporary(&pending, file, &bytes, Step::WriteTemp, Step::SyncTemp)?;

    let (backup, backup_file) = create_temporary(root, Step::CreateBackupTemp)?;
    fill_temporary(
        &backup,
        backup_file,
        original,
        Step::WriteBackupTemp,
        Step::SyncBackupTemp,
    )?;
    checkpoint(
        Step::ReplaceBackup,
        "sostituzione del backup",
        &root.path().join(CONFIG_BACKUP_FILE),
    )?;
    root.replace_file(backup.name(), OsStr::new(CONFIG_BACKUP_FILE))
        .map_err(|error| ConfigError::write("sostituzione del backup", error))?;
    backup.keep();

    checkpoint(
        Step::PublishMain,
        "sostituzione del principale",
        &root.path().join(CONFIG_FILE),
    )?;
    root.replace_file(pending.name(), OsStr::new(CONFIG_FILE))
        .map_err(|error| ConfigError::write("sostituzione del principale", error))?;
    pending.keep();

    Ok((finish(root, next.revision()), preserved))
}

/// Recupero persistito dal backup: conserva il principale corrotto, se esiste,
/// e installa la generazione recuperata con la sostituzione atomica.
///
/// La revisione è quella del backup; `config.json.prev` non viene modificato e
/// resta la generazione recuperata.
pub(super) fn recover(
    root: &VerifiedRoot,
    backup: ConfigV1,
    main: &GenerationFailure,
) -> Result<(Commit, Option<OsString>), ConfigError> {
    root.verify_path().map_err(ConfigError::Protection)?;
    let bytes = read_generation(root, CONFIG_BACKUP_FILE)?;

    let preserved = match main {
        GenerationFailure::Corrupt { .. } => Some(preserve_corrupt(root)?),
        GenerationFailure::Absent => None,
    };

    let (pending, file) = create_temporary(root, Step::CreateTemp)?;
    fill_temporary(&pending, file, &bytes, Step::WriteTemp, Step::SyncTemp)?;
    checkpoint(
        Step::PublishMain,
        "sostituzione del principale",
        &root.path().join(CONFIG_FILE),
    )?;
    root.replace_file(pending.name(), OsStr::new(CONFIG_FILE))
        .map_err(|error| ConfigError::write("sostituzione del principale", error))?;
    pending.keep();

    Ok((finish(root, backup.revision()), preserved))
}

fn preserve_corrupt(root: &VerifiedRoot) -> Result<OsString, ConfigError> {
    let bytes = read_generation(root, CONFIG_FILE)?;
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    checkpoint(
        Step::PreserveCorrupt,
        "conservazione del principale corrotto",
        root.path(),
    )?;

    for attempt in 0..MAX_PRESERVE_ATTEMPTS {
        let name = OsString::from(format!("{CORRUPT_PREFIX}{epoch}.{attempt}"));
        match root.create_file(&name) {
            Ok(file) => {
                let pending = PendingFile::new(root, name);
                fill_temporary(&pending, file, &bytes, Step::WriteTemp, Step::SyncTemp)?;
                return Ok(pending.keep());
            }
            Err(PlatformError::AlreadyExists { .. }) => continue,
            Err(error) => {
                return Err(ConfigError::write(
                    "conservazione del principale corrotto",
                    error,
                ));
            }
        }
    }
    Err(ConfigError::TemporaryCollision {
        attempts: MAX_PRESERVE_ATTEMPTS,
    })
}

/// Rimuove i temporanei del namespace riservato al modulo.
///
/// Ogni nome che corrisponde esattamente al modello `.config.<pid>.<n>.tmp`
/// viene rimosso, indipendentemente dal PID: con la precondizione di un solo
/// scrittore non esistono temporanei attivi di altri processi. Gli altri file,
/// comprese le copie `config.corrupt.*`, i file dell'utente e i nomi simili ma
/// fuori dal namespace, non vengono toccati. Un nome del namespace occupato da
/// una directory produce un errore esplicito.
pub(super) fn discard_reserved_temporaries(
    root: &VerifiedRoot,
) -> Result<Vec<OsString>, ConfigError> {
    let names = root
        .list_file_names()
        .map_err(|error| ConfigError::write("lettura della radice", error))?;
    let mut removed = Vec::new();
    for name in names {
        if !is_reserved_temporary(&name) {
            continue;
        }
        match root.remove_file(&name) {
            Ok(()) => removed.push(name),
            Err(PlatformError::FileAbsent { .. }) => {}
            Err(error) => return Err(ConfigError::write("rimozione del temporaneo", error)),
        }
    }
    removed.sort();
    Ok(removed)
}

fn read_generation(root: &VerifiedRoot, name: &str) -> Result<Vec<u8>, ConfigError> {
    match read_bounded(root, name)? {
        Some(bytes) => Ok(bytes),
        None => Err(ConfigError::Protection(PlatformError::FileAbsent {
            path: root.path().join(name),
        })),
    }
}

#[cfg(test)]
pub(super) mod fault {
    //! Guasti simulati per thread, attivi soltanto nelle prove della libreria.
    //!
    //! Due modi, entrambi armati per thread: un errore restituito al passo
    //! (`arm`) oppure la terminazione immediata del processo senza unwinding
    //! (`arm_crash`). L'arresto serve alle prove in processi figli; nessuno dei
    //! due meccanismi esiste nei binari operativi.

    use std::cell::Cell;

    use super::Step;

    thread_local! {
        static ARMED: Cell<Option<Step>> = const { Cell::new(None) };
        static CRASH: Cell<Option<Step>> = const { Cell::new(None) };
    }

    pub(in crate::config) fn arm(step: Step) {
        ARMED.with(|armed| armed.set(Some(step)));
    }

    pub(in crate::config) fn clear() {
        ARMED.with(|armed| armed.set(None));
    }

    pub(super) fn armed(step: Step) -> bool {
        ARMED.with(|armed| armed.get() == Some(step))
    }

    /// Arma la terminazione immediata al passo indicato, senza rimuovere
    /// temporanei e senza eseguire `Drop`.
    pub(in crate::config) fn arm_crash(step: Step) {
        CRASH.with(|armed| armed.set(Some(step)));
    }

    pub(super) fn crash_armed(step: Step) -> bool {
        CRASH.with(|armed| armed.get() == Some(step))
    }
}
