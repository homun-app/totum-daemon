//! Registro interno D4. redb gestisce il formato fisico e le transazioni;
//! Totum possiede schema, limiti, apertura, bootstrap, posizioni, snapshot e
//! retention. Nessun percorso viene aperto da redb: il backend riceve soltanto
//! file creati o aperti e verificati da [`platform`].
//!
//! Posizioni ed epoch:
//!
//! - una posizione interna è la coppia `(epoch, seq)`; `seq = 0` è la posizione
//!   vuota iniziale, il primo batch assegna da 1 e la retention non riutilizza
//!   mai una sequenza rimossa;
//! - l'epoch è persistita nella stessa transazione dei metadati che la
//!   introducono, resta invariata a riapertura, recupero, snapshot e retention,
//!   e soltanto [`Journal::reset`] ne crea una diversa;
//! - una lettura con epoch diversa riceve [`JournalError::EpochChanged`], una
//!   posizione sotto il floor riceve [`JournalError::RetentionExpired`] e una
//!   posizione oltre l'ultimo evento riceve [`JournalError::PositionBeyondEnd`]:
//!   non esistono salti silenziosi;
//! - l'overflow della sequenza è verificato prima di ogni mutazione e produce
//!   [`JournalError::SequenceExhausted`], quello dell'epoch
//!   [`JournalError::EpochExhausted`]: nessun avvolgimento e nessun reset
//!   automatico.
//!
//! Apertura di un journal esistente:
//!
//! 1. il file viene aperto in lettura e scrittura senza crearlo, con le
//!    verifiche di tipo, proprietario, protezioni e collegamenti di `platform`;
//! 2. un file vuoto o illeggibile è un rifiuto: non autorizza una nuova
//!    inizializzazione e non viene mai troncato o sostituito;
//! 3. la classificazione avviene con un'apertura **in sola lettura**: schema
//!    futuro, schema precedente migrabile e metadati Totum incoerenti sono
//!    riconosciuti senza scritture e il file resta identico byte per byte;
//! 4. un journal il cui ultimo commit redb non è leggibile senza ripristino è
//!    un esito distinto: l'apertura in scrittura ricostruisce lo stato interno
//!    del motore (unica mutazione prima della convalida Totum), poi la stessa
//!    convalida decide e la riapertura normale segnala la modifica effettuata;
//! 5. uno schema 1 precedente alla fondazione delle posizioni viene migrato
//!    allo schema 2 in un'unica transazione che conserva ogni evento; se gli
//!    eventi non sono contigui alla sequenza dichiarata la migrazione non è
//!    possibile e il file viene rifiutato senza scritture;
//! 6. soltanto dopo la convalida i temporanei del namespace riservato vengono
//!    scartati.
//!
//! Snapshot e retention:
//!
//! - uno snapshot dichiara epoch, schema del blob e ultima sequenza applicata,
//!   ed è prodotto da un reducer sintetico deterministico a partire da una
//!   vista coerente degli eventi, riprendendo lo snapshot precedente della
//!   stessa epoch;
//! - la retention elimina soltanto un prefisso coperto da uno snapshot valido,
//!   aggiorna il floor nella stessa transazione della rimozione e non promette
//!   una riduzione immediata della dimensione fisica del file redb.
//!
//! Bootstrap alla prima esecuzione o dopo un bootstrap interrotto:
//!
//! 1. un file assente con residui `.journal.<pid>.<contatore>.tmp` è un
//!    bootstrap interrotto: i residui vengono scartati, mai pubblicati;
//! 2. viene creato un temporaneo in lettura e scrittura con protezioni
//!    verificate;
//! 3. redb inizializza il file e Totum scrive schema, epoch iniziale, floor a
//!    zero e posizione vuota in un'unica transazione, che ne conferma il commit
//!    e la sincronizzazione dei dati;
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
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use redb::{
    BackendError, Database, ReadOnlyDatabase, ReadableDatabase, ReadableTable,
    ReadableTableMetadata, StorageBackend, TableDefinition,
};

use crate::platform::{
    self, Consumer, DirectorySync, PlatformError, RootClaim, SharedRoot, VerifiedRoot,
};

/// Nome del file del journal nella radice di stato.
pub const JOURNAL_FILE: &str = "journal.redb";
/// Versione dello schema Totum scritta da questo incremento.
///
/// Lo schema 2 aggiunge epoch, floor, tempi di ingestione, snapshot e la
/// verifica delle posizioni. Lo schema 1 è quello scritto dalla baseline del
/// primo modulo; viene migrato conservando ogni evento.
pub const SCHEMA_VERSION: u64 = 2;
/// Schema precedente compreso come sorgente di una migrazione in avanti.
pub const LEGACY_SCHEMA_VERSION: u64 = 1;

/// Namespace riservato ai temporanei di bootstrap del journal.
const TEMP_PREFIX: &str = ".journal.";
const TEMP_SUFFIX: &str = ".tmp";
const MAX_TEMP_ATTEMPTS: u32 = 16;
const CACHE_SIZE: usize = 16 * 1024 * 1024;

const META: TableDefinition<&str, u64> = TableDefinition::new("metadata");
const EVENTS: TableDefinition<u64, &[u8]> = TableDefinition::new("events");
/// Tempo di ingestione assegnato dall'hub per ogni evento, in millisecondi
/// dall'epoch Unix. La retention usa questo valore e l'orologio iniettato dal
/// chiamante, mai un timestamp del runtime dell'agente.
const TIMES: TableDefinition<u64, u64> = TableDefinition::new("times");
/// Snapshot sintetici indicizzati per ultima sequenza applicata.
const SNAPSHOTS: TableDefinition<u64, &[u8]> = TableDefinition::new("snapshots");

const META_SCHEMA: &str = "schema";
const META_LAST: &str = "last";
const META_FLOOR: &str = "floor";
const META_EPOCH: &str = "epoch";
const META_INGESTED: &str = "ingestedAt";

const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_BATCH: usize = 16;
const MAX_PAGE: usize = 16;

mod retention;
mod snapshot;

pub use retention::{RetentionLimits, RetentionOutcome};
pub use snapshot::{ReducerState, Snapshot};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(0);

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
    EpochExhausted,
    /// La posizione appartiene a un'epoch diversa da quella corrente: gli
    /// eventi di una generazione precedente non vengono mai reinterpretati.
    EpochChanged {
        expected: u64,
        found: u64,
    },
    /// La posizione precede il floor: il prefisso è stato rimosso dalla
    /// retention e non esiste un salto silenzioso.
    RetentionExpired {
        position: Position,
        floor: u64,
    },
    /// La posizione supera l'ultimo evento della stessa vista coerente.
    PositionBeyondEnd {
        position: Position,
        high_water: u64,
    },
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
            JournalError::EpochExhausted => {
                write!(f, "epoch del journal esaurita: nessun reset")
            }
            JournalError::EpochChanged { expected, found } => write!(
                f,
                "epoch del journal cambiata: attesa {expected}, ricevuta {found}; la posizione non viene reinterpretata"
            ),
            JournalError::RetentionExpired { position, floor } => write!(
                f,
                "posizione ({}, {}) precedente al floor {floor}: gli eventi sono stati rimossi dalla retention",
                position.epoch, position.seq
            ),
            JournalError::PositionBeyondEnd {
                position,
                high_water,
            } => write!(
                f,
                "posizione ({}, {}) oltre l'ultimo evento {high_water}: nessuna lettura in avanti",
                position.epoch, position.seq
            ),
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
    /// Schema precedente migrato in questa apertura, se ne esisteva uno. Gli
    /// eventi conservati non cambiano sequenza.
    pub migrated_from: Option<u64>,
}

/// Posizione interna nel journal: l'epoch della generazione e la sequenza
/// dell'evento. `seq = 0` è la posizione vuota iniziale.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Position {
    pub epoch: u64,
    pub seq: u64,
}

impl Position {
    pub fn new(epoch: u64, seq: u64) -> Self {
        Self { epoch, seq }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Event {
    pub sequence: u64,
    pub payload: Vec<u8>,
}

/// Pagina letta da una posizione, con epoch, floor e high-water della stessa
/// vista coerente: gli append successivi non cambiano la pagina già letta.
#[derive(Debug, PartialEq, Eq)]
pub struct Page {
    pub epoch: u64,
    pub floor: u64,
    pub high_water: u64,
    pub events: Vec<Event>,
}

/// Stato osservabile del journal, senza payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JournalStats {
    /// Epoch corrente: cambia soltanto con un reset esplicito.
    pub epoch: u64,
    /// Ultima sequenza rimossa dalla retention; gli eventi conservati sono
    /// `(floor, high_water]`.
    pub floor: u64,
    /// Ultima sequenza assegnata.
    pub high_water: u64,
    /// Numero di eventi conservati.
    pub events: u64,
    /// Somma dei byte dei payload conservati: byte logici, non dimensione del
    /// file redb.
    pub logical_bytes: u64,
    /// Dimensione fisica corrente del file, misurata sull'handle verificato e
    /// aperto, non sul percorso nominale. redb non riduce il file in modo
    /// immediato dopo una rimozione: il valore è osservato, non promesso, ed è
    /// diagnostico, quindi `None` se il sistema non la fornisce.
    pub physical_bytes: Option<u64>,
}

pub struct Journal {
    db: Database,
    recovery_required: bool,
    // Copia dell'handle verificato e consegnato al backend: la misura fisica
    // legge questo file, anche quando il percorso nominale non lo identifica
    // più. Nessuna operazione riapre il percorso.
    opened_file: File,
    // Percorso nominale del principale, usato soltanto per la diagnostica dei
    // punti di arresto. Nessuna operazione lo riapre.
    path: PathBuf,
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

        // La classificazione apre il motore in sola lettura: schema futuro,
        // schema precedente migrabile e corruzione vengono riconosciuti senza
        // scrivere, quindi il file resta identico byte per byte. Solo un
        // journal che il motore non riesce a leggere senza ripristino prosegue
        // in scrittura.
        let copy = file
            .try_clone()
            .map_err(|error| JournalError::Platform(PlatformError::io(&path, error)))?;
        let repaired = match classify(&root, copy, &path) {
            Classification::Refused(error) => return Err(error),
            Classification::Current | Classification::Legacy => false,
            Classification::Recovery => true,
        };

        // Apertura in scrittura senza controllo d'integrità globale: quel
        // controllo riscrive il file a ogni apertura, mentre la classificazione
        // ha già letto schema, albero di sistema e stato dell'allocatore. Se il
        // motore ha segnalato il ripristino, è qui che ricostruisce il proprio
        // stato; la migrazione o la convalida Totum seguono comunque il
        // ripristino.
        let opened_file = file
            .try_clone()
            .map_err(|error| JournalError::Platform(PlatformError::io(&path, error)))?;
        let db = database(&root, file, path.clone()).map_err(|error| match error {
            JournalError::Storage(error) => {
                corrupt(format!("apertura del motore fallita: {error}"))
            }
            other => other,
        })?;
        let migrated_from = match observe(&db)? {
            Observation::Current => None,
            Observation::Legacy => {
                migrate_legacy(&db, &path)?;
                Some(LEGACY_SCHEMA_VERSION)
            }
        };
        if migrated_from.is_some() {
            check_current(&db)?;
        }

        let discarded = discard_temporaries(root.verified_root())?;
        Ok((
            Journal {
                db,
                recovery_required: false,
                opened_file,
                path,
                _claim: claim,
            },
            Startup {
                kind: StartupKind::Existing { repaired },
                discarded_temporaries: discarded,
                migrated_from,
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
            meta.insert(META_SCHEMA, SCHEMA_VERSION).map_err(storage)?;
            meta.insert(META_LAST, 0u64).map_err(storage)?;
            meta.insert(META_FLOOR, 0u64).map_err(storage)?;
            meta.insert(META_EPOCH, generate_epoch()).map_err(storage)?;
            meta.insert(META_INGESTED, now_millis()).map_err(storage)?;
            transaction.open_table(EVENTS).map_err(storage)?;
            transaction.open_table(TIMES).map_err(storage)?;
            transaction.open_table(SNAPSHOTS).map_err(storage)?;
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
        let path = root.path().join(JOURNAL_FILE);
        let opened_file = file
            .try_clone()
            .map_err(|error| JournalError::Platform(PlatformError::io(&path, error)))?;
        let db = database(&root, file, path.clone()).map_err(|error| match error {
            JournalError::Storage(error) => {
                corrupt(format!("riapertura dopo il bootstrap fallita: {error}"))
            }
            other => other,
        })?;
        check_current(&db)?;

        let kind = if residue.is_empty() {
            StartupKind::FirstRun { publication }
        } else {
            StartupKind::InterruptedBootstrap { publication }
        };
        Ok((
            Journal {
                db,
                recovery_required: false,
                opened_file,
                path,
                _claim: claim,
            },
            Startup {
                kind,
                discarded_temporaries: residue,
                migrated_from: None,
            },
        ))
    }

    /// Scrive un batch e la sua posizione in un'unica transazione.
    /// L'intervallo indica il commit redb; la durabilità della voce di
    /// directory resta un contratto separato.
    ///
    /// Ogni evento riceve il tempo di ingestione assegnato qui, mai inferiore a
    /// quello dell'evento precedente: un orologio che torna indietro non
    /// anticipa la retention. L'overflow della sequenza è verificato prima di
    /// qualunque scrittura, dentro la stessa transazione.
    pub fn append(
        &mut self,
        payloads: &[&[u8]],
    ) -> Result<std::ops::RangeInclusive<u64>, JournalError> {
        self.append_ingested(payloads, now_millis())
    }

    /// Append con tempo di ingestione esplicito. Il tempo viene comunque
    /// limitato a quello dell'evento precedente; le prove lo usano per rendere
    /// deterministiche le soglie della retention.
    fn append_ingested(
        &mut self,
        payloads: &[&[u8]],
        assigned: u64,
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
            let current = require_meta(&meta, META_LAST)?;
            let last = current
                .checked_add(payloads.len() as u64)
                .ok_or(JournalError::SequenceExhausted)?;
            let previous = meta
                .get(META_INGESTED)
                .map_err(storage)?
                .map(|value| value.value())
                .unwrap_or(0);
            let ingested = assigned.max(previous);
            let first = current + 1;
            let mut events = tx.open_table(EVENTS).map_err(storage)?;
            let mut times = tx.open_table(TIMES).map_err(storage)?;
            for (sequence, payload) in (first..=last).zip(payloads.iter().copied()) {
                events.insert(sequence, payload).map_err(storage)?;
                times.insert(sequence, ingested).map_err(storage)?;
            }
            meta.insert(META_LAST, last).map_err(storage)?;
            meta.insert(META_INGESTED, ingested).map_err(storage)?;
            (first, last)
        };
        if let Err(error) = tx.commit() {
            self.recovery_required = true;
            return Err(JournalError::CommitOutcomeUnknown(error));
        }
        Ok(first..=last)
    }

    /// Legge una pagina coerente a partire da una posizione.
    ///
    /// Epoch, floor e high-water sono letti nella stessa transazione degli
    /// eventi. Una posizione di un'altra epoch, sotto il floor o oltre l'ultimo
    /// evento produce un errore esplicito: nessun salto silenzioso.
    pub fn read_from(&self, position: Position, limit: usize) -> Result<Page, JournalError> {
        if self.recovery_required {
            return Err(JournalError::RecoveryRequired);
        }
        if limit == 0 || limit > MAX_PAGE {
            return Err(JournalError::InvalidPage);
        }
        let tx = self.db.begin_read().map_err(storage)?;
        let meta = tx.open_table(META).map_err(storage)?;
        let epoch = require_meta(&meta, META_EPOCH)?;
        let floor = require_meta(&meta, META_FLOOR)?;
        let high_water = require_meta(&meta, META_LAST)?;
        if position.epoch != epoch {
            return Err(JournalError::EpochChanged {
                expected: epoch,
                found: position.epoch,
            });
        }
        if position.seq < floor {
            return Err(JournalError::RetentionExpired { position, floor });
        }
        if position.seq > high_water {
            return Err(JournalError::PositionBeyondEnd {
                position,
                high_water,
            });
        }
        let events = tx.open_table(EVENTS).map_err(storage)?;
        let page = events
            .range((Bound::Excluded(position.seq), Bound::Included(high_water)))
            .map_err(storage)?
            .take(limit)
            .map(|entry| {
                let (key, value) = entry.map_err(storage)?;
                Ok(Event {
                    sequence: key.value(),
                    payload: value.value().to_vec(),
                })
            })
            .collect::<Result<Vec<_>, JournalError>>()?;
        Ok(Page {
            epoch,
            floor,
            high_water,
            events: page,
        })
    }

    /// Legge una pagina dopo la sequenza indicata nell'epoch corrente.
    pub fn read_after(&self, sequence: u64, limit: usize) -> Result<Vec<Event>, JournalError> {
        let epoch = self.epoch()?;
        Ok(self
            .read_from(
                Position {
                    epoch,
                    seq: sequence,
                },
                limit,
            )?
            .events)
    }

    /// Epoch corrente. Resta invariata a riapertura, recupero, snapshot e
    /// retention.
    pub fn epoch(&self) -> Result<u64, JournalError> {
        if self.recovery_required {
            return Err(JournalError::RecoveryRequired);
        }
        let tx = self.db.begin_read().map_err(storage)?;
        let meta = tx.open_table(META).map_err(storage)?;
        require_meta(&meta, META_EPOCH)
    }

    /// Posizione dell'ultimo evento assegnato: `(epoch, high_water)`.
    pub fn end_position(&self) -> Result<Position, JournalError> {
        Ok(Position {
            epoch: self.epoch()?,
            seq: self.statistics()?.high_water,
        })
    }

    /// Stato osservabile del journal, con byte logici e fisici distinti.
    pub fn statistics(&self) -> Result<JournalStats, JournalError> {
        if self.recovery_required {
            return Err(JournalError::RecoveryRequired);
        }
        let tx = self.db.begin_read().map_err(storage)?;
        let meta = tx.open_table(META).map_err(storage)?;
        let epoch = require_meta(&meta, META_EPOCH)?;
        let floor = require_meta(&meta, META_FLOOR)?;
        let high_water = require_meta(&meta, META_LAST)?;
        let events = tx.open_table(EVENTS).map_err(storage)?;
        let mut count = 0u64;
        let mut logical_bytes = 0u64;
        for entry in events
            .range((Bound::Excluded(floor), Bound::Included(high_water)))
            .map_err(storage)?
        {
            let (_, value) = entry.map_err(storage)?;
            count += 1;
            logical_bytes += value.value().len() as u64;
        }
        Ok(JournalStats {
            epoch,
            floor,
            high_water,
            events: count,
            logical_bytes,
            physical_bytes: self.physical_bytes(),
        })
    }

    /// Crea una nuova epoch e azzera eventi, floor, tempi e snapshot in
    /// un'unica transazione.
    ///
    /// È l'unica operazione che cambia l'epoch. Le posizioni della generazione
    /// precedente vengono rifiutate con [`JournalError::EpochChanged`]; non
    /// esiste un reset automatico, una migrazione che cambia epoch o un
    /// recupero che ne inventa una.
    pub fn reset(&mut self) -> Result<u64, JournalError> {
        if self.recovery_required {
            return Err(JournalError::RecoveryRequired);
        }
        let tx = self.db.begin_write().map_err(storage)?;
        let epoch = {
            let mut meta = tx.open_table(META).map_err(storage)?;
            let current = require_meta(&meta, META_EPOCH)?;
            let epoch = current.checked_add(1).ok_or(JournalError::EpochExhausted)?;
            {
                let mut events = tx.open_table(EVENTS).map_err(storage)?;
                events.retain(|_, _| false).map_err(storage)?;
                let mut times = tx.open_table(TIMES).map_err(storage)?;
                times.retain(|_, _| false).map_err(storage)?;
                let mut snapshots = tx.open_table(SNAPSHOTS).map_err(storage)?;
                snapshots.retain(|_, _| false).map_err(storage)?;
            }
            meta.insert(META_EPOCH, epoch).map_err(storage)?;
            meta.insert(META_LAST, 0u64).map_err(storage)?;
            meta.insert(META_FLOOR, 0u64).map_err(storage)?;
            meta.insert(META_INGESTED, now_millis()).map_err(storage)?;
            epoch
        };
        checkpoint(Step::ResetBeforeCommit, &self.path)?;
        if let Err(error) = tx.commit() {
            self.recovery_required = true;
            return Err(JournalError::CommitOutcomeUnknown(error));
        }
        checkpoint(Step::ResetAfterCommit, &self.path)?;
        Ok(epoch)
    }

    /// Dimensione fisica del file aperto dal backend, in byte.
    ///
    /// La lettura usa l'handle verificato e consegnato al backend, mai il
    /// percorso nominale: una rinomina o una sostituzione del nome non cambia
    /// il file misurato. La misura è diagnostica: se il sistema non la
    /// fornisce vale `None` e non trasforma in errore un'operazione già
    /// committata.
    fn physical_bytes(&self) -> Option<u64> {
        #[cfg(test)]
        if fault::measurement_failing() {
            return None;
        }
        self.opened_file
            .metadata()
            .ok()
            .map(|metadata| metadata.len())
    }
}

/// Legge un metadato obbligatorio o rifiuta il journal.
fn require_meta(
    meta: &impl ReadableTable<&'static str, u64>,
    key: &str,
) -> Result<u64, JournalError> {
    meta.get(key)
        .map_err(storage)?
        .map(|value| value.value())
        .ok_or_else(|| JournalError::Corrupt {
            detail: format!("metadato `{key}` assente"),
        })
}

/// Esito dell'apertura in sola lettura di un journal esistente.
enum Classification {
    /// Schema Totum corrente, stato del motore leggibile senza ripristino.
    Current,
    /// Schema precedente migrabile: gli eventi conservati verranno mantenuti
    /// nella stessa sequenza dalla migrazione in avanti.
    Legacy,
    /// Il motore non legge l'ultimo commit senza ricostruire il proprio stato:
    /// l'apertura in scrittura eseguirà il ripristino, che è una mutazione del
    /// formato prima della convalida Totum.
    Recovery,
    /// Rifiuto già deciso: il file non è stato toccato.
    Refused(JournalError),
}

/// Schema osservato in un database aperto, senza scritture.
enum Observation {
    Current,
    Legacy,
}

/// Classifica il journal esistente senza scrivere.
///
/// L'apertura in sola lettura non ripara né modifica il file: un journal con
/// schema futuro, migrazione impossibile o metadati Totum incoerenti viene
/// rifiutato con i byte originali intatti. Un errore del motore diverso dal
/// ripristino necessario è una corruzione; `RepairAborted` è l'unico esito che
/// autorizza l'apertura in scrittura successiva.
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
    let verdict = observe(&db);
    drop(db);
    match verdict {
        Ok(Observation::Current) => Classification::Current,
        Ok(Observation::Legacy) => Classification::Legacy,
        Err(error) => Classification::Refused(error),
    }
}

/// Determina lo schema del database aperto e ne verifica gli invarianti senza
/// scrivere. Uno schema 1 è accettato soltanto se la migrazione è possibile.
fn observe(db: &impl ReadableDatabase) -> Result<Observation, JournalError> {
    let schema = {
        let transaction = db.begin_read().map_err(corrupt)?;
        let meta = transaction.open_table(META).map_err(corrupt)?;
        require_meta(&meta, META_SCHEMA)?
    };
    if schema > SCHEMA_VERSION {
        return Err(JournalError::FutureSchema { found: schema });
    }
    match schema {
        SCHEMA_VERSION => {
            check_current(db)?;
            Ok(Observation::Current)
        }
        LEGACY_SCHEMA_VERSION => {
            check_legacy(db)?;
            Ok(Observation::Legacy)
        }
        other => Err(corrupt(format!(
            "schema del journal {other} non riconosciuto"
        ))),
    }
}

/// Verifica gli invarianti applicativi dello schema corrente.
///
/// Gli eventi conservati devono essere esattamente `(floor, high_water]`, senza
/// buchi né residui; ogni evento ha il proprio tempo di ingestione, in ordine
/// non decrescente; gli snapshot decodificano, appartengono alla stessa epoch e
/// restano nel range conservato. Se il floor è avanzato esiste almeno uno
/// snapshot che copre il prefisso rimosso.
fn check_current(db: &impl ReadableDatabase) -> Result<(), JournalError> {
    let transaction = db.begin_read().map_err(corrupt)?;
    let meta = transaction.open_table(META).map_err(corrupt)?;
    let epoch = require_meta(&meta, META_EPOCH)?;
    let floor = require_meta(&meta, META_FLOOR)?;
    let high_water = require_meta(&meta, META_LAST)?;
    if epoch == 0 {
        return Err(corrupt("epoch del journal nulla"));
    }
    if floor > high_water {
        return Err(corrupt("floor oltre l'ultimo evento"));
    }

    let events = transaction.open_table(EVENTS).map_err(corrupt)?;
    let times = transaction.open_table(TIMES).map_err(corrupt)?;
    let retained = high_water - floor;
    if events.len().map_err(corrupt)? != retained {
        return Err(corrupt(
            "eventi conservati non coerenti con floor e high-water",
        ));
    }
    if times.len().map_err(corrupt)? != retained {
        return Err(corrupt("tempi di ingestione non coerenti con gli eventi"));
    }
    if retained > 0 {
        let mut previous_time = 0u64;
        for (index, entry) in events
            .range((Bound::Excluded(floor), Bound::Included(high_water)))
            .map_err(corrupt)?
            .enumerate()
        {
            let expected = floor + 1 + index as u64;
            let (key, _) = entry.map_err(corrupt)?;
            if key.value() != expected {
                return Err(corrupt("sequenza degli eventi non contigua"));
            }
            let time = times
                .get(expected)
                .map_err(corrupt)?
                .ok_or_else(|| JournalError::Corrupt {
                    detail: format!("tempo di ingestione assente per la sequenza {expected}"),
                })?
                .value();
            if index > 0 && time < previous_time {
                return Err(corrupt("tempi di ingestione in ordine decrescente"));
            }
            previous_time = time;
        }
    }

    let snapshots = transaction.open_table(SNAPSHOTS).map_err(corrupt)?;
    let mut count = 0u64;
    let mut covered = false;
    for entry in snapshots.range(..).map_err(corrupt)? {
        let (key, value) = entry.map_err(corrupt)?;
        let record = snapshot::decode(value.value()).ok_or_else(|| JournalError::Corrupt {
            detail: "snapshot non decodificabile".to_owned(),
        })?;
        if record.epoch != epoch {
            return Err(corrupt("snapshot di un'epoch diversa"));
        }
        if record.applied_through != key.value() {
            return Err(corrupt("snapshot con posizione incoerente"));
        }
        if record.applied_through < floor || record.applied_through > high_water {
            return Err(corrupt("snapshot fuori dal range conservato"));
        }
        covered = true;
        count += 1;
    }
    if count > snapshot::MAX_SNAPSHOTS as u64 {
        return Err(corrupt("troppi snapshot conservati"));
    }
    if floor > 0 && !covered {
        return Err(corrupt("prefisso rimosso senza uno snapshot valido"));
    }
    Ok(())
}

/// Verifica che uno schema 1 sia migrabile senza scritture: ogni evento
/// dichiarato dalla posizione deve esistere e le sequenze devono essere
/// contigue da 1.
fn check_legacy(db: &impl ReadableDatabase) -> Result<(), JournalError> {
    let transaction = db.begin_read().map_err(corrupt)?;
    let meta = transaction.open_table(META).map_err(corrupt)?;
    let last = require_meta(&meta, META_LAST)?;
    let events = transaction.open_table(EVENTS).map_err(corrupt)?;
    if events.len().map_err(corrupt)? != last {
        return Err(corrupt(
            "migrazione impossibile: numero di eventi diverso dalla posizione dichiarata",
        ));
    }
    let mut expected = 1u64;
    for entry in events.range(..).map_err(corrupt)? {
        let (key, _) = entry.map_err(corrupt)?;
        if key.value() != expected {
            return Err(corrupt(
                "migrazione impossibile: sequenza degli eventi non contigua",
            ));
        }
        expected = match expected.checked_add(1) {
            Some(next) => next,
            None => break,
        };
    }
    Ok(())
}

/// Migra uno schema 1 allo schema corrente in un'unica transazione.
///
/// Gli eventi non vengono riscritti: lo schema 1 non registrava il tempo di
/// ingestione, quindi la migrazione assegna a tutti gli eventi conservati il
/// proprio istante e introduce epoch, floor a zero e la tabella degli snapshot.
/// Ogni evento mantiene la sequenza. Un errore di commit è un esito sconosciuto
/// che richiede la riapertura, senza ritentativi automatici.
fn migrate_legacy(db: &Database, path: &Path) -> Result<(), JournalError> {
    let transaction = db.begin_write().map_err(storage)?;
    {
        let mut meta = transaction.open_table(META).map_err(storage)?;
        let schema = require_meta(&meta, META_SCHEMA)?;
        if schema != LEGACY_SCHEMA_VERSION {
            return Err(corrupt(format!(
                "schema {schema} inatteso durante la migrazione"
            )));
        }
        let last = require_meta(&meta, META_LAST)?;
        {
            let events = transaction.open_table(EVENTS).map_err(storage)?;
            let mut count = 0u64;
            for entry in events.range(..).map_err(storage)? {
                let (key, _) = entry.map_err(storage)?;
                count = count.saturating_add(1);
                if key.value() != count {
                    return Err(corrupt(
                        "migrazione impossibile: sequenza degli eventi non contigua",
                    ));
                }
            }
            if count != last {
                return Err(corrupt(
                    "migrazione impossibile: numero di eventi diverso dalla posizione dichiarata",
                ));
            }
        }

        let ingested = now_millis();
        meta.insert(META_EPOCH, generate_epoch()).map_err(storage)?;
        meta.insert(META_FLOOR, 0u64).map_err(storage)?;
        meta.insert(META_INGESTED, ingested).map_err(storage)?;
        meta.insert(META_SCHEMA, SCHEMA_VERSION).map_err(storage)?;
        let mut times = transaction.open_table(TIMES).map_err(storage)?;
        for sequence in 1..=last {
            times.insert(sequence, ingested).map_err(storage)?;
        }
        transaction.open_table(SNAPSHOTS).map_err(storage)?;
    }
    checkpoint(Step::MigrateBeforeCommit, path)?;
    if let Err(error) = transaction.commit() {
        return Err(JournalError::CommitOutcomeUnknown(error));
    }
    Ok(())
}

/// Generatore dell'epoch: identificatore opaco, non un'identità crittografica.
///
/// Combina orologio, processo e un contatore locale, senza dipendenze
/// aggiuntive. Zero non è mai un'epoch valida.
fn generate_epoch() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let counter = NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
    let mut value = nanos
        ^ u64::from(std::process::id()).rotate_left(32)
        ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 31;
    if value == 0 { 1 } else { value }
}

/// Istante corrente in millisecondi dall'epoch Unix, saturato.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
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
    MigrateBeforeCommit,
    SnapshotBeforeCommit,
    SnapshotAfterCommit,
    RetainBeforeCommit,
    RetainAfterCommit,
    ResetBeforeCommit,
    ResetAfterCommit,
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
        static MEASURE_FAILURE: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn arm_crash(step: Step) {
        CRASH.with(|armed| armed.set(Some(step)));
    }

    pub(super) fn crash_armed(step: Step) -> bool {
        CRASH.with(|armed| armed.get() == Some(step))
    }

    /// Rende indisponibile la misura fisica, come farebbe un sistema che non
    /// la fornisce: serve a dimostrare che un esito già committato non
    /// diventa un errore.
    pub(super) fn fail_measurement(failing: bool) {
        MEASURE_FAILURE.with(|flag| flag.set(failing));
    }

    pub(super) fn measurement_failing() -> bool {
        MEASURE_FAILURE.with(Cell::get)
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests;
