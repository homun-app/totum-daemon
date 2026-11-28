//! Errori tipizzati della configurazione.
//!
//! Ogni rifiuto nomina il percorso e la causa. Nessun errore produce un valore
//! predefinito o una riparazione. Un errore di scrittura restituito prima del
//! punto di commit significa che la generazione precedente è rimasta intatta;
//! un guasto successivo al commit non è un errore ma un esito
//! `Commit::CommittedNotDurable`.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use super::{Component, ComponentState, GenerationFailure, MigrationSource};
use crate::platform::PlatformError;

/// Rifiuto della classificazione o della scrittura.
#[derive(Debug)]
pub enum ConfigError {
    /// Lettura fallita dopo l'apertura verificata.
    Read { path: PathBuf, source: io::Error },
    /// La primitiva di piattaforma ha rifiutato collegamento, tipo,
    /// proprietario o protezioni.
    Protection(PlatformError),
    /// Il file supera il limite di lettura.
    TooLarge { path: PathBuf, limit: u64 },
    /// Lo schema è successivo a quello compreso: nessuna reinterpretazione.
    TooNew { path: PathBuf, schema_version: u64 },
    /// La revisione non è un intero non negativo.
    InvalidRevision { path: PathBuf, detail: String },
    /// La revisione del principale è inferiore a quella del backup.
    RevisionRegression { main: u64, backup: u64 },
    /// La revisione del principale su disco non è quella caricata: il
    /// salvataggio non adotta né sovrascrive la generazione divergente.
    RevisionDivergence { expected: u64, found: u64 },
    /// Il principale su disco è assente o corrotto: il salvataggio richiede un
    /// recupero esplicito e non lo applica.
    RecoveryRequired { main: GenerationFailure },
    /// Il principale su disco usa uno schema precedente e va migrato prima di
    /// un ulteriore salvataggio; un salvataggio non applica la migrazione.
    MigrationRequired {
        from_schema_version: u64,
        source: MigrationSource,
    },
    /// Lo schema del principale è precedente a quello del backup: migrare il
    /// principale scarterebbe una generazione più nuova. Nessuna scrittura.
    SchemaRegression { main: u64, backup: u64 },
    /// La generazione con schema precedente non è più quella classificata:
    /// la migrazione non viene applicata. Si verifica soltanto se lo stato
    /// cambia per vie che non rispettano il lock.
    MigrationSourceChanged { source: MigrationSource },
    /// La revisione dello schema precedente è al massimo e non può essere
    /// incrementata: la migrazione è rifiutata prima di ogni scrittura.
    MigrationRevisionOverflow { revision: u64 },
    /// Né il principale né il backup sono una generazione v1 utilizzabile.
    NoValidGeneration {
        main: GenerationFailure,
        backup: GenerationFailure,
    },
    /// Identità o registro hanno uno stato non ammesso.
    ComponentState {
        component: Component,
        state: ComponentState,
    },
    /// Esiste una configurazione ma manca un componente di stato.
    IncoherentState { component: Component },
    /// La serializzazione dell'intestazione è fallita; non dovrebbe accadere
    /// con i soli campi numerici dello schema v1.
    Serialize { detail: String },
    /// Un'operazione di scrittura, sincronizzazione o rinomina è fallita prima
    /// del punto di commit: la generazione precedente è intatta e nessun
    /// salvataggio è avvenuto.
    Write {
        action: &'static str,
        source: PlatformError,
    },
    /// La revisione non può essere incrementata: ha raggiunto il massimo.
    RevisionOverflow { revision: u64 },
    /// I nomi dei file temporanei sono esauriti per collisioni ripetute.
    TemporaryCollision { attempts: u32 },
    /// `config.json` è comparso durante la prima creazione: la destinazione non
    /// viene sovrascritta e serve una nuova classificazione.
    AppearedDuringFirstRun { path: PathBuf },
    /// Un altro [`Store`](crate::config::Store) è già aperto sulla stessa
    /// radice: una seconda istanza avrebbe il proprio mutex e non
    /// serializzerebbe con la prima le scritture.
    AlreadyOpen { path: PathBuf },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Read { path, source } => {
                write!(f, "lettura fallita per {}: {source}", path.display())
            }
            ConfigError::Protection(error) => {
                write!(f, "file di configurazione rifiutato: {error}")
            }
            ConfigError::TooLarge { path, limit } => write!(
                f,
                "configurazione oltre il limite di {limit} byte: {}",
                path.display()
            ),
            ConfigError::TooNew {
                path,
                schema_version,
            } => write!(
                f,
                "schema {schema_version} successivo a 1 in {}: nessuna reinterpretazione",
                path.display()
            ),
            ConfigError::InvalidRevision { path, detail } => {
                write!(f, "revisione non valida in {}: {detail}", path.display())
            }
            ConfigError::RevisionRegression { main, backup } => write!(
                f,
                "revisione del principale {main} inferiore a quella del backup {backup}"
            ),
            ConfigError::RevisionDivergence { expected, found } => write!(
                f,
                "revisione del principale {found} diversa da quella attesa {expected}: salvataggio rifiutato"
            ),
            ConfigError::RecoveryRequired { main } => write!(
                f,
                "salvataggio rifiutato: principale {}; il recupero va richiesto esplicitamente",
                failure_label(main)
            ),
            ConfigError::MigrationRequired {
                from_schema_version,
                source,
            } => write!(
                f,
                "schema {from_schema_version} precedente nel {}: migrazione necessaria prima di continuare",
                source.label()
            ),
            ConfigError::SchemaRegression { main, backup } => write!(
                f,
                "schema del principale {main} precedente a quello del backup {backup}: nessuna migrazione"
            ),
            ConfigError::MigrationSourceChanged { source } => write!(
                f,
                "la generazione con schema precedente nel {} non è più quella classificata: migrazione rifiutata",
                source.label()
            ),
            ConfigError::MigrationRevisionOverflow { revision } => write!(
                f,
                "revisione {revision} dello schema precedente al massimo: migrazione rifiutata senza scrivere"
            ),
            ConfigError::NoValidGeneration { main, backup } => write!(
                f,
                "nessuna generazione valida: principale {}, backup {}",
                failure_label(main),
                failure_label(backup)
            ),
            ConfigError::ComponentState { component, state } => write!(
                f,
                "{} in stato non ammesso in D3: {}",
                component.label(),
                state.label()
            ),
            ConfigError::IncoherentState { component } => write!(
                f,
                "configurazione presente senza {}: stato incoerente",
                component.label()
            ),
            ConfigError::Serialize { detail } => {
                write!(f, "serializzazione della configurazione fallita: {detail}")
            }
            ConfigError::Write { action, source } => {
                write!(f, "scrittura non commessa durante {action}: {source}")
            }
            ConfigError::RevisionOverflow { revision } => write!(
                f,
                "revisione {revision} al massimo: incremento rifiutato senza scrivere"
            ),
            ConfigError::TemporaryCollision { attempts } => write!(
                f,
                "nessun nome temporaneo disponibile dopo {attempts} tentativi"
            ),
            ConfigError::AppearedDuringFirstRun { path } => write!(
                f,
                "{} è comparso durante la prima creazione: nessuna sovrascrittura",
                path.display()
            ),
            ConfigError::AlreadyOpen { path } => write!(
                f,
                "configurazione già aperta su questa radice ({}): una seconda istanza non è ammessa",
                path.display()
            ),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ConfigError::Read { source, .. } => Some(source),
            ConfigError::Protection(error) => Some(error),
            ConfigError::Write { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl ConfigError {
    pub(super) fn write(action: &'static str, source: PlatformError) -> Self {
        ConfigError::Write { action, source }
    }
}

fn failure_label(failure: &GenerationFailure) -> &str {
    match failure {
        GenerationFailure::Absent => "assente",
        GenerationFailure::Corrupt { .. } => "corrotto",
    }
}
