//! Parsing, validazione e serializzazione dei documenti di configurazione.
//!
//! Un documento di configurazione è sempre un oggetto JSON: array, scalari e
//! `null` sono corrotti, mai una generazione valida o migrabile. Il parsing
//! avviene in due fasi: prima la sola intestazione, per riconoscere uno schema
//! futuro senza reinterpretarlo; poi il documento completo della versione
//! riconosciuta. La v1 è validata con campi sconosciuti, duplicati e mancanti
//! rifiutati e con la revisione validata come intero non negativo. Lo schema
//! precedente è soltanto quello sintetico 0, con il solo `schemaVersion`: un
//! contenuto diverso è corrotto e non viene mai migrato. La trasformazione
//! `v0 → v1` è una funzione pura, separata dal file system. La serializzazione
//! produce soltanto l'intestazione v1. Il modulo non conosce il file system e
//! non modifica nulla.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use super::{CONFIG_SCHEMA_VERSION, ConfigError, ConfigV0, ConfigV1, MIGRATABLE_SCHEMA_VERSION};

/// Esito del parsing di un file.
pub(super) enum Parsed {
    Current(ConfigV1),
    /// Schema precedente riconosciuto, validato e migrabile.
    Migratable(ConfigV0),
    TooNew {
        schema_version: u64,
    },
    Invalid {
        detail: String,
    },
    InvalidRevision {
        detail: String,
    },
}

#[derive(Deserialize)]
struct Header {
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
}

/// Il documento v1 completo. `deny_unknown_fields` rende un campo sconosciuto
/// un errore; i campi duplicati sono rifiutati da serde.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(rename = "schemaVersion")]
    _schema_version: u64,
    revision: serde_json::Value,
}

/// Il documento v0 sintetico: soltanto `schemaVersion`, nient'altro. Un campo
/// aggiuntivo, compresa una `revision`, rende il documento non migrabile.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentV0 {
    #[serde(rename = "schemaVersion")]
    _schema_version: u64,
}

/// Esige la forma oggetto prima di interpretare lo schema.
///
/// La deserializzazione derivata accetterebbe anche una sequenza posizionale,
/// per esempio `[0]` come `{"schemaVersion":0}`: lo schema ammette invece
/// soltanto l'oggetto, quindi array, scalari e `null` sono corrotti e mai
/// sorgenti migrabili. Il controllo guarda il primo carattere dopo lo spazio
/// JSON bianco e non costruisce un valore intermedio: i campi duplicati restano
/// visibili alla deserializzazione e vengono rifiutati come prima.
fn is_object(bytes: &[u8]) -> bool {
    matches!(
        bytes
            .iter()
            .copied()
            .find(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r')),
        Some(b'{')
    )
}

pub(super) fn parse(bytes: &[u8]) -> Parsed {
    if !is_object(bytes) {
        return Parsed::Invalid {
            detail: "atteso un oggetto JSON in posizione di documento".to_owned(),
        };
    }

    let header: Header = match serde_json::from_slice(bytes) {
        Ok(header) => header,
        Err(error) => {
            return Parsed::Invalid {
                detail: error.to_string(),
            };
        }
    };

    match header.schema_version.cmp(&CONFIG_SCHEMA_VERSION) {
        Ordering::Greater => {
            return Parsed::TooNew {
                schema_version: header.schema_version,
            };
        }
        Ordering::Less => {
            // Sotto la v1 esiste soltanto lo schema sintetico 0. Se il
            // contenuto non è esattamente il suo documento, il file è corrotto
            // e non viene mai migrato.
            return match serde_json::from_slice::<DocumentV0>(bytes) {
                Ok(_) => Parsed::Migratable(ConfigV0::initial()),
                Err(error) => Parsed::Invalid {
                    detail: format!("schema {MIGRATABLE_SCHEMA_VERSION} non migrabile: {error}"),
                },
            };
        }
        Ordering::Equal => {}
    }

    let document: Document = match serde_json::from_slice(bytes) {
        Ok(document) => document,
        Err(error) => {
            return Parsed::Invalid {
                detail: error.to_string(),
            };
        }
    };

    match document.revision.as_u64() {
        Some(revision) => Parsed::Current(ConfigV1::with_revision(revision)),
        None => Parsed::InvalidRevision {
            detail: format!(
                "atteso un intero non negativo, trovato {}",
                json_kind(&document.revision)
            ),
        },
    }
}

/// Trasformazione pura e verificabile dello schema sintetico 0 nella v1.
///
/// Il contenuto di dominio è vuoto in entrambi, quindi la trasformazione
/// conserva tutto ciò che esiste e incrementa la revisione di uno: la
/// generazione migrata è la successiva, non una riscrittura della precedente.
/// L'incremento fallisce soltanto al massimo di `u64`, caso che la fixture
/// sintetica non raggiunge ma che resta esplicito.
pub(super) fn migrate_v0_to_v1(previous: ConfigV0) -> Result<ConfigV1, ConfigError> {
    let revision =
        previous
            .revision()
            .checked_add(1)
            .ok_or(ConfigError::MigrationRevisionOverflow {
                revision: previous.revision(),
            })?;
    Ok(ConfigV1::with_revision(revision))
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "un booleano",
        serde_json::Value::Number(_) => "un numero non intero o fuori intervallo",
        serde_json::Value::String(_) => "una stringa",
        serde_json::Value::Array(_) => "un elenco",
        serde_json::Value::Object(_) => "un oggetto",
    }
}

/// Documento v1 in scrittura: soltanto l'intestazione, con i nomi esatti.
#[derive(Serialize)]
struct Outgoing {
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
    revision: u64,
}

/// Serializza la generazione con la revisione indicata, con un a capo finale.
pub(super) fn serialize(revision: u64) -> Result<Vec<u8>, ConfigError> {
    let document = Outgoing {
        schema_version: CONFIG_SCHEMA_VERSION,
        revision,
    };
    serde_json::to_vec(&document)
        .map(|mut bytes| {
            bytes.push(b'\n');
            bytes
        })
        .map_err(|error| ConfigError::Serialize {
            detail: error.to_string(),
        })
}
