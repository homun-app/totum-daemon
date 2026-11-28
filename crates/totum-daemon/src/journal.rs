//! Registro interno D4. redb gestisce il formato fisico e le transazioni;
//! Totum possiede schema, limiti, apertura e bootstrap del file. Nessun
//! percorso viene aperto da redb: il backend riceve soltanto file creati o
//! aperti e verificati da [`platform`].
//!
//! Apertura di un journal esistente:
//!
//! 1. il file viene aperto in lettura e scrittura senza crearlo, con le
//!    verifiche di tipo, proprietario, protezioni e collegamenti di `platform`;
//! 2. un file vuoto o illeggibile è un rifiuto: non autorizza una nuova
//!    inizializzazione e non viene mai troncato o sostituito;
//! 3. la classificazione avviene con un'apertura **in sola lettura**: schema
//!    futuro e metadati Totum incoerenti sono riconosciuti senza scritture e
//!    il file resta identico byte per byte;
//! 4. un journal il cui ultimo commit redb non è leggibile senza ripristino è
//!    un esito distinto: l'apertura in scrittura ricostruisce lo stato interno
//!    del motore (unica mutazione prima della convalida Totum), poi la stessa
//!    convalida decide e la riapertura normale segnala la modifica effettuata;
//! 5. soltanto dopo la convalida i temporanei del namespace riservato vengono
//!    scartati.
//!
//! Bootstrap alla prima esecuzione o dopo un bootstrap interrotto:
//!
//! 1. un file assente con residui `.journal.<pid>.<contatore>.tmp` è un
//!    bootstrap interrotto: i residui vengono scartati, mai pubblicati;
//! 2. viene creato un temporaneo in lettura e scrittura con protezioni
//!    verificate;
//! 3. redb inizializza il file e Totum scrive schema e posizione iniziale in
//!    un'unica transazione, che ne conferma il commit e la sincronizzazione
//!    dei dati;
//! 4. il database viene chiuso prima della pubblicazione, così il file non è
//!    aperto durante la rinomina;
//! 5. il temporaneo viene pubblicato come `journal.redb` senza sostituzione;
//! 6. la voce di directory viene sincronizzata e l'esito distingue la
//!    durabilità confermata da quella non confermata.
//!
//! Un errore di commit redb non è un permesso per ritentare: l'esito è
//! sconosciuto e richiede la riapertura. Un errore della pubblicazione prima
//! del punto di rinomina è classificato come non commesso e lascia al più il
//! temporaneo, che la riapertura successiva riconosce e scarta.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use redb::{
    BackendError, Database, ReadOnlyDatabase, ReadableDatabase, ReadableTable, StorageBackend,
    TableDefinition,
};

use crate::platform::{
    self, Consumer, DirectorySync, PlatformError, RootClaim, SharedRoot, VerifiedRoot,
};

/// Nome del file del journal nella radice di stato.
pub const JOURNAL_FILE: &str = "journal.redb";
/// Versione dello schema Totum compresa da questo incremento.
pub const SCHEMA_VERSION: u64 = 1;

/// Namespace riservato ai temporanei di bootstrap del journal.
const TEMP_PREFIX: &str = ".journal.";
const TEMP_SUFFIX: &str = ".tmp";
const MAX_TEMP_ATTEMPTS: u32 = 16;
const CACHE_SIZE: usize = 16 * 1024 * 1024;

const META: TableDefinition<&str, u64> = TableDefinition::new("metadata");
const EVENTS: TableDefinition<u64, &[u8]> = TableDefinition::new("events");
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_BATCH: usize = 16;
const MAX_PAGE: usize = 16;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Backend redb legato a un file già verificato da Totum.
///
/// Trattiene un [`SharedRoot`]: il lock della radice resta acquisito finché il
/// database e quindi il backend sono vivi. Il percorso serve soltanto alla
/// diagnostica; nessuna operazione riapre il file.
#[derive(Debug)]
struct FileBackend {
    file: Mutex<File>,
    // Trattenuto per l'intera vita del database: il lock resta acquisito.
    _root: SharedRoot,
    path: PathBuf,
}

impl FileBackend {
    fn file(&self) -> Result<std::sync::MutexGuard<'_, File>, BackendError> {
        self.file
            .lock()
            .map_err(|_| BackendError::Io(io::Error::other("journal file lock poisoned")))
    }
}

impl StorageBackend for FileBackend {
    fn len(&self) -> Result<u64, BackendError> {
        Ok(self.file()?.metadata()?.len())
    }

    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), BackendError> {
        let mut file = self.file()?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(out)?;
        Ok(())
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), BackendError> {
        let mut file = self.file()?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)?;
        Ok(())
    }

    fn set_len(&self, len: u64) -> Result<(), BackendError> {
        self.file()?.set_len(len)?;
        Ok(())
    }

    fn sync_data(&self) -> Result<(), BackendError> {
        let file = self.file()?;
        platform::sync_file(&file, &self.path).map_err(|error| BackendError::Other(Box::new(error)))
    }
}

/// Errore del journal, senza panici e senza ripieghi permissivi.
#[derive(Debug)]
pub enum JournalError {
    Platform(PlatformError),
    /// Il file esiste ma non contiene un registro interpretabile: schema
    /// Totum mancante, metadati incoerenti o formato non aperto dal motore.
    Corrupt {
        detail: String,
    },
    /// Schema Totum successivo a quello compreso: rifiuto senza inizializzazione.
    FutureSchema {
        found: u64,
    },
    /// Errore del motore in una fase senza punto di commit raggiunto.
    Storage(Box<dyn std::error::Error + Send + Sync>),
    InvalidBatch,
    InvalidPage,
    SequenceExhausted,
    /// Il commit redb è fallito dopo un punto in cui non è possibile sapere se
    /// il batch è stato applicato: serve la riapertura, senza ritentare.
    CommitOutcomeUnknown(redb::CommitError),
    /// La destinazione è comparsa durante il bootstrap: nessuna sovrascrittura.
    AppearedDuringBootstrap {
        path: PathBuf,
    },
    /// Un altro journal è già aperto sulla stessa radice: la seconda apertura
    /// non arriva al motore né al file.
    AlreadyOpen {
        path: PathBuf,
    },
    RecoveryRequired,
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JournalError::Platform(error) => write!(f, "{error}"),
            JournalError::Corrupt { detail } => {
                write!(f, "journal inutilizzabile: {detail}")
            }
            JournalError::FutureSchema { found } => write!(
                f,
                "schema del journal {found} successivo a {SCHEMA_VERSION}: nessuna inizializzazione"
            ),
            JournalError::Storage(error) => write!(f, "errore del motore del journal: {error}"),
            JournalError::InvalidBatch => write!(
                f,
                "batch del journal non valido: vuoto, oltre {MAX_BATCH} record o con un record vuoto o oltre {MAX_RECORD_BYTES} byte"
            ),
            JournalError::InvalidPage => {
                write!(f, "limite di pagina non valido: ammesso da 1 a {MAX_PAGE}")
            }
            JournalError::SequenceExhausted => {
                write!(f, "sequenza del journal esaurita: nessun avanzamento")
            }
            JournalError::CommitOutcomeUnknown(error) => write!(
                f,
                "esito del commit del journal sconosciuto ({error}): serve la riapertura, nessun nuovo batch"
            ),
            JournalError::AppearedDuringBootstrap { path } => write!(
                f,
                "journal comparso durante il bootstrap: {} non viene sovrascritto",
                path.display()
            ),
            JournalError::AlreadyOpen { path } => write!(
                f,
                "journal già aperto su questa radice ({}): una seconda istanza non è ammessa",
                path.display()
            ),
            JournalError::RecoveryRequired => {
                write!(f, "journal in attesa di riapertura: nessun nuovo batch")
            }
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JournalError::Platform(error) => Some(error),
            JournalError::Storage(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

fn corrupt(error: impl std::fmt::Display) -> JournalError {
    JournalError::Corrupt {
        detail: error.to_string(),
    }
}

fn storage(error: impl std::error::Error + Send + Sync + 'static) -> JournalError {
    JournalError::Storage(Box::new(error))
}

/// Descrive come è stato reso disponibile il file del journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Publication {
    /// Pubblicazione e sincronizzazione della voce di directory confermate.
    Durable,
    /// Pubblicazione avvenuta, durabilità della voce di directory non
    /// confermata: non è un errore e non viene ritentata.
    NotDurable { detail: String },
}

impl Publication {
    /// Indica se la piattaforma ha confermato la durabilità della pubblicazione.
    pub fn durable(&self) -> bool {
        matches!(self, Publication::Durable)
    }
}

/// Come è stato ottenuto il file del journal all'apertura.
#[derive(Debug)]
pub enum StartupKind {
    /// Nessun file né residuo di bootstrap: `journal.redb` è stato creato.
    FirstRun { publication: Publication },
    /// Esistevano soltanto residui riconoscibili di un bootstrap interrotto:
    /// sono stati scartati e `journal.redb` è stato creato.
    InterruptedBootstrap { publication: Publication },
    /// `journal.redb` esisteva ed è stato riaperto in loco: nessuna nuova
    /// generazione e nessuna inizializzazione.
    Existing {
        /// Il motore ha ricostruito il proprio stato interno all'apertura.
        /// È una mutazione del formato redb, non dei dati Totum: schema,
        /// posizione ed eventi non vengono reinterpretati né riscritti.
        repaired: bool,
    },
}

/// Esito dell'apertura del journal.
#[derive(Debug)]
pub struct Startup {
    pub kind: StartupKind,
    /// Temporanei del namespace riservato rimossi prima dell'apertura.
    pub discarded_temporaries: Vec<OsString>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Event {
    pub sequence: u64,
    pub payload: Vec<u8>,
}

pub struct Journal {
    db: Database,
    recovery_required: bool,
    // Occupazione della radice per tutta la vita del journal: trattiene il lock
    // e impedisce una seconda apertura, che non arriverebbe al file.
    _claim: RootClaim,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("recovery_required", &self.recovery_required)
            .finish_non_exhaustive()
    }
}

impl Journal {
    /// Apre o inizializza il journal sotto la radice bloccata.
    ///
    /// L'accesso trattiene il lock: il journal e la configurazione condividono
    /// lo stesso [`SharedRoot`] e il lock resta acquisito finché l'ultimo
    /// utilizzatore è vivo. Una seconda apertura sulla stessa radice è
    /// [`JournalError::AlreadyOpen`] e avviene prima di qualunque accesso al
    /// file, perché due journal indipendenti non serializzerebbero i batch.
    /// La classificazione distingue prima esecuzione, bootstrap interrotto,
    /// journal esistente, schema futuro e corruzione; un file vuoto o
    /// illeggibile non viene mai azzerato.
    pub fn open(root: impl Into<SharedRoot>) -> Result<(Journal, Startup), JournalError> {
        let root = root.into();
        let path = root.path().join(JOURNAL_FILE);
        let claim = root
            .claim(Consumer::Journal)
            .ok_or_else(|| JournalError::AlreadyOpen { path: path.clone() })?;
        match root
            .verified_root()
            .open_existing_state_file(OsStr::new(JOURNAL_FILE), true)
        {
            Ok(file) => Self::open_existing(root, claim, path, file),
            Err(PlatformError::FileAbsent { .. }) => Self::bootstrap(root, claim),
            Err(error) => Err(JournalError::Platform(error)),
        }
    }

    fn open_existing(
        root: SharedRoot,
        claim: RootClaim,
        path: PathBuf,
        file: File,
    ) -> Result<(Journal, Startup), JournalError> {
        let length = file
            .metadata()
            .map_err(|error| JournalError::Platform(PlatformError::io(&path, error)))?
            .len();
        if length == 0 {
            return Err(JournalError::Corrupt {
                detail: "file vuoto: un journal vuoto non autorizza una nuova inizializzazione"
                    .to_owned(),
            });
        }

        // La classificazione apre il motore in sola lettura: schema futuro e
        // corruzione vengono riconosciuti senza scrivere, quindi il file resta
        // identico byte per byte. Solo un journal che il motore non riesce a
        // leggere senza ripristino prosegue in scrittura.
        let copy = file
            .try_clone()
            .map_err(|error| JournalError::Platform(PlatformError::io(&path, error)))?;
        let repaired = match classify(&root, copy, &path) {
            Classification::Refused(error) => return Err(error),
            Classification::Readable => false,
            Classification::Recovery => true,
        };

        // Apertura in scrittura senza controllo d'integrità globale: quel
        // controllo riscrive il file a ogni apertura, mentre la classificazione
        // ha già letto schema, albero di sistema e stato dell'allocatore. Se il
        // motore ha segnalato il ripristino, è qui che ricostruisce il proprio
        // stato; la convalida Totum segue comunque il ripristino.
        let db = database(&root, file, path.clone()).map_err(|error| match error {
            JournalError::Storage(error) => {
                corrupt(format!("apertura del motore fallita: {error}"))
            }
            other => other,
        })?;
        validate(&db)?;

        let discarded = discard_temporaries(root.verified_root())?;
        Ok((
            Journal {
                db,
                recovery_required: false,
                _claim: claim,
            },
            Startup {
                kind: StartupKind::Existing { repaired },
                discarded_temporaries: discarded,
            },
        ))
    }

    fn bootstrap(root: SharedRoot, claim: RootClaim) -> Result<(Journal, Startup), JournalError> {
        let verified = root.verified_root();
        let residue = reserved_temporaries(verified)?;
        for name in &residue {
            match verified.remove_file(name) {
                Ok(()) | Err(PlatformError::FileAbsent { .. }) => {}
                Err(error) => return Err(JournalError::Platform(error)),
            }
        }

        let mut temp = create_temporary(verified)?;
        let temp_path = verified.path().join(temp.name());

        checkpoint(Step::OpenDatabase, &temp_path)?;
        let db = database(&root, temp.file(), temp_path.clone())?;

        checkpoint(Step::CommitSchema, &temp_path)?;
        let transaction = db.begin_write().map_err(storage)?;
        {
            let mut meta = transaction.open_table(META).map_err(storage)?;
            meta.insert("schema", SCHEMA_VERSION).map_err(storage)?;
            meta.insert("last", 0u64).map_err(storage)?;
            transaction.open_table(EVENTS).map_err(storage)?;
        }
        transaction
            .commit()
            .map_err(JournalError::CommitOutcomeUnknown)?;

        // La rinomina avviene soltanto a database chiuso: su Windows un handle
        // aperto impedirebbe la pubblicazione.
        drop(db);

        checkpoint(Step::PublishMain, &temp_path)?;
        match verified.publish_file(temp.name(), OsStr::new(JOURNAL_FILE)) {
            Ok(()) => {}
            Err(PlatformError::AlreadyExists { path }) => {
                return Err(JournalError::AppearedDuringBootstrap { path });
            }
            Err(error) => return Err(JournalError::Platform(error)),
        }
        temp.keep();

        checkpoint(Step::SyncDirectory, verified.path())?;
        let publication = finish_publication(verified);

        let file = verified
            .open_existing_state_file(OsStr::new(JOURNAL_FILE), true)
            .map_err(JournalError::Platform)?;
        let db =
            database(&root, file, root.path().join(JOURNAL_FILE)).map_err(|error| match error {
                JournalError::Storage(error) => {
                    corrupt(format!("riapertura dopo il bootstrap fallita: {error}"))
                }
                other => other,
            })?;
        validate(&db)?;

        let kind = if residue.is_empty() {
            StartupKind::FirstRun { publication }
        } else {
            StartupKind::InterruptedBootstrap { publication }
        };
        Ok((
            Journal {
                db,
                recovery_required: false,
                _claim: claim,
            },
            Startup {
                kind,
                discarded_temporaries: residue,
            },
        ))
    }

    /// Scrive un batch e la sua posizione in un'unica transazione.
    /// L'intervallo indica il commit redb; la durabilità della voce di
    /// directory resta un contratto separato.
    pub fn append(
        &mut self,
        payloads: &[&[u8]],
    ) -> Result<std::ops::RangeInclusive<u64>, JournalError> {
        if self.recovery_required {
            return Err(JournalError::RecoveryRequired);
        }
        if payloads.is_empty()
            || payloads.len() > MAX_BATCH
            || payloads
                .iter()
                .any(|payload| payload.is_empty() || payload.len() > MAX_RECORD_BYTES)
        {
            return Err(JournalError::InvalidBatch);
        }
        let tx = self.db.begin_write().map_err(storage)?;
        let (first, last) = {
            let mut meta = tx.open_table(META).map_err(storage)?;
            let current = meta
                .get("last")
                .map_err(storage)?
                .ok_or_else(|| JournalError::Corrupt {
                    detail: "posizione iniziale assente".to_owned(),
                })?
                .value();
            let last = current
                .checked_add(payloads.len() as u64)
                .ok_or(JournalError::SequenceExhausted)?;
            let first = current + 1;
            let mut events = tx.open_table(EVENTS).map_err(storage)?;
            for (sequence, payload) in (first..=last).zip(payloads.iter().copied()) {
                events.insert(sequence, payload).map_err(storage)?;
            }
            meta.insert("last", last).map_err(storage)?;
            (first, last)
        };
        if let Err(error) = tx.commit() {
            self.recovery_required = true;
            return Err(JournalError::CommitOutcomeUnknown(error));
        }
        Ok(first..=last)
    }

    /// Legge una pagina coerente, dopo la posizione indicata.
    pub fn read_after(&self, sequence: u64, limit: usize) -> Result<Vec<Event>, JournalError> {
        if self.recovery_required {
            return Err(JournalError::RecoveryRequired);
        }
        if limit == 0 || limit > MAX_PAGE {
            return Err(JournalError::InvalidPage);
        }
        let tx = self.db.begin_read().map_err(storage)?;
        let events = tx.open_table(EVENTS).map_err(storage)?;
        events
            .range((
                std::ops::Bound::Excluded(sequence),
                std::ops::Bound::Unbounded,
            ))
            .map_err(storage)?
            .take(limit)
            .map(|entry| {
                let (key, value) = entry.map_err(storage)?;
                Ok(Event {
                    sequence: key.value(),
                    payload: value.value().to_vec(),
                })
            })
            .collect()
    }
}

/// Esito dell'apertura in sola lettura di un journal esistente.
enum Classification {
    /// Schema Totum valido e stato del motore leggibile senza ripristino.
    Readable,
    /// Il motore non legge l'ultimo commit senza ricostruire il proprio stato:
    /// l'apertura in scrittura eseguirà il ripristino, che è una mutazione del
    /// formato prima della convalida Totum.
    Recovery,
    /// Rifiuto già deciso: il file non è stato toccato.
    Refused(JournalError),
}

/// Classifica il journal esistente senza scrivere.
///
/// L'apertura in sola lettura non ripara né modifica il file: un journal con
/// schema futuro o metadati Totum incoerenti viene rifiutato con i byte
/// originali intatti. Un errore del motore diverso dal ripristino necessario è
/// una corruzione; `RepairAborted` è l'unico esito che autorizza l'apertura in
/// scrittura successiva.
fn classify(root: &SharedRoot, file: File, path: &Path) -> Classification {
    let db = match read_only(root, file, path.to_owned()) {
        Ok(db) => db,
        Err(redb::DatabaseError::RepairAborted) => return Classification::Recovery,
        Err(redb::DatabaseError::DatabaseAlreadyOpen) => {
            return Classification::Refused(JournalError::AlreadyOpen {
                path: path.to_owned(),
            });
        }
        Err(error) => {
            return Classification::Refused(corrupt(format!(
                "apertura in sola lettura fallita: {error}"
            )));
        }
    };
    let verdict = validate(&db);
    drop(db);
    match verdict {
        Ok(()) => Classification::Readable,
        Err(error) => Classification::Refused(error),
    }
}

fn read_only(
    root: &SharedRoot,
    file: File,
    path: PathBuf,
) -> Result<ReadOnlyDatabase, redb::DatabaseError> {
    let mut builder = Database::builder();
    builder.set_cache_size(CACHE_SIZE);
    builder.open_read_only_with_backend(FileBackend {
        file: Mutex::new(file),
        _root: root.clone(),
        path,
    })
}

fn database(root: &SharedRoot, file: File, path: PathBuf) -> Result<Database, JournalError> {
    let mut builder = Database::builder();
    builder.set_cache_size(CACHE_SIZE);
    builder
        .create_with_backend(FileBackend {
            file: Mutex::new(file),
            _root: root.clone(),
            path: path.clone(),
        })
        .map_err(|error| match error {
            // Difesa in profondità: l'occupazione della radice impedisce già
            // una seconda apertura nello stesso processo.
            redb::DatabaseError::DatabaseAlreadyOpen => JournalError::AlreadyOpen { path },
            error => storage(error),
        })
}

/// Verifica i metadati Totum del database aperto, senza scrivere.
fn validate(db: &impl ReadableDatabase) -> Result<(), JournalError> {
    let transaction = db.begin_read().map_err(corrupt)?;
    let meta = transaction.open_table(META).map_err(corrupt)?;
    let schema = meta
        .get("schema")
        .map_err(corrupt)?
        .map(|value| value.value());
    let position = meta.get("last").map_err(corrupt)?;
    transaction.open_table(EVENTS).map_err(corrupt)?;

    let Some(schema) = schema else {
        return Err(JournalError::Corrupt {
            detail: "metadato dello schema assente".to_owned(),
        });
    };
    if schema > SCHEMA_VERSION {
        return Err(JournalError::FutureSchema { found: schema });
    }
    if schema != SCHEMA_VERSION {
        return Err(JournalError::Corrupt {
            detail: format!("schema del journal {schema} non riconosciuto"),
        });
    }
    if position.is_none() {
        return Err(JournalError::Corrupt {
            detail: "posizione iniziale assente".to_owned(),
        });
    }
    Ok(())
}

fn finish_publication(root: &VerifiedRoot) -> Publication {
    match root.sync_directory() {
        Ok(DirectorySync::Synced) => Publication::Durable,
        Ok(DirectorySync::Unsupported) => Publication::NotDurable {
            detail: "la piattaforma non sincronizza la voce di directory".to_owned(),
        },
        Err(error) => Publication::NotDurable {
            detail: error.to_string(),
        },
    }
}

fn next_temp_name() -> OsString {
    let counter = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    OsString::from(format!(
        "{TEMP_PREFIX}{}.{counter}{TEMP_SUFFIX}",
        std::process::id()
    ))
}

/// Riconosce un nome del namespace `.journal.<pid>.<contatore>.tmp`.
///
/// Il PID fa parte del nome, non è un criterio di proprietà: con un solo
/// scrittore ogni nome del namespace è un residuo di bootstrap da scartare.
fn is_reserved_temporary(name: &OsStr) -> bool {
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

fn reserved_temporaries(root: &VerifiedRoot) -> Result<Vec<OsString>, JournalError> {
    let names = root.list_file_names().map_err(JournalError::Platform)?;
    let mut reserved: Vec<OsString> = names
        .into_iter()
        .filter(|name| is_reserved_temporary(name))
        .collect();
    reserved.sort();
    Ok(reserved)
}

fn discard_temporaries(root: &VerifiedRoot) -> Result<Vec<OsString>, JournalError> {
    let reserved = reserved_temporaries(root)?;
    for name in &reserved {
        match root.remove_file(name) {
            Ok(()) | Err(PlatformError::FileAbsent { .. }) => {}
            Err(error) => return Err(JournalError::Platform(error)),
        }
    }
    Ok(reserved)
}

/// Temporaneo creato ma non ancora pubblicato: se non viene disarmato, il
/// `Drop` lo rimuove. Un processo terminato senza unwinding lascia il residuo,
/// che la riapertura successiva riconosce e scarta.
struct Temporary<'a> {
    root: &'a VerifiedRoot,
    name: OsString,
    file: Option<File>,
    keep: bool,
}

impl<'a> Temporary<'a> {
    fn name(&self) -> &OsStr {
        &self.name
    }

    fn file(&mut self) -> File {
        self.file
            .take()
            .expect("descrittore del temporaneo già consumato")
    }

    fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = self.root.remove_file(&self.name);
        }
    }
}

fn create_temporary(root: &VerifiedRoot) -> Result<Temporary<'_>, JournalError> {
    checkpoint(Step::CreateTemp, root.path())?;
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let name = next_temp_name();
        match root.create_read_write_file(&name) {
            Ok(file) => {
                return Ok(Temporary {
                    root,
                    name,
                    file: Some(file),
                    keep: false,
                });
            }
            Err(PlatformError::AlreadyExists { .. }) => continue,
            Err(error) => return Err(JournalError::Platform(error)),
        }
    }
    Err(JournalError::Platform(PlatformError::AlreadyExists {
        path: root.path().to_path_buf(),
    }))
}

/// Punti di arresto deterministici del bootstrap, attivi soltanto nelle prove
/// della libreria; non esistono nel binario operativo.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    CreateTemp,
    OpenDatabase,
    CommitSchema,
    PublishMain,
    SyncDirectory,
}

fn checkpoint(step: Step, path: &Path) -> Result<(), JournalError> {
    #[cfg(test)]
    if fault::crash_armed(step) {
        std::process::abort();
    }
    let _ = (step, path);
    Ok(())
}

#[cfg(test)]
mod fault {
    use std::cell::Cell;

    use super::Step;

    thread_local! {
        static CRASH: Cell<Option<Step>> = const { Cell::new(None) };
    }

    pub(super) fn arm_crash(step: Step) {
        CRASH.with(|armed| armed.set(Some(step)));
    }

    pub(super) fn crash_armed(step: Step) -> bool {
        CRASH.with(|armed| armed.get() == Some(step))
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests;
