//! Formati versionati minimi di `identity.json` e `controllers.json`.
//!
//! Entrambi i documenti sono oggetti JSON con `schemaVersion: 1` e i soli campi
//! definiti qui. Un campo sconosciuto, un campo duplicato, un tipo errato o un
//! documento troncato rendono il file inutilizzabile. Una versione diversa da 1
//! è uno schema non compreso: in P7 non esiste alcuna migrazione dei file di
//! identità. Il modulo non conosce il file system e non modifica nulla.
//!
//! I documenti iniziali non contengono chiavi, impronte, token o voci di
//! controller: lo schema delle voci future non è definito e appartiene a D12.

use serde::{Deserialize, Serialize};

use super::{CONTROLLERS_SCHEMA_VERSION, IDENTITY_SCHEMA_VERSION, INITIAL_STATE};

/// Esito del parsing di `identity.json`.
pub(super) enum ParsedIdentity {
    Initial,
    NonInitial { detail: String },
    UnsupportedSchema { schema_version: u64 },
    Invalid { detail: String },
}

/// Esito del parsing di `controllers.json`.
pub(super) enum ParsedControllers {
    Initial,
    NonInitial { detail: String },
    UnsupportedSchema { schema_version: u64 },
    Invalid { detail: String },
}

#[derive(Deserialize)]
struct Header {
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityV1 {
    #[serde(rename = "schemaVersion")]
    _schema_version: u64,
    state: String,
}

/// Il registro iniziale ha `grantRevision: 0` e un elenco vuoto. L'elenco è
/// letto come valori JSON perché lo schema delle voci non è definito: una lista
/// non vuota è un contenuto non iniziale, non un documento interpretabile.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControllersV1 {
    #[serde(rename = "schemaVersion")]
    _schema_version: u64,
    #[serde(rename = "grantRevision")]
    grant_revision: u64,
    controllers: Vec<serde_json::Value>,
}

/// Impone la forma oggetto prima di interpretare lo schema.
///
/// La deserializzazione derivata accetterebbe una sequenza posizionale, per
/// esempio `[1]`; i formati ammettono soltanto l'oggetto. Il controllo guarda
/// il primo carattere dopo lo spazio JSON bianco senza costruire un valore
/// intermedio, quindi i campi duplicati restano visibili e vengono rifiutati.
fn is_object(bytes: &[u8]) -> bool {
    matches!(
        bytes
            .iter()
            .copied()
            .find(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r')),
        Some(b'{')
    )
}

fn schema_version(bytes: &[u8]) -> Option<u64> {
    serde_json::from_slice::<Header>(bytes)
        .ok()
        .map(|header| header.schema_version)
}

pub(super) fn parse_identity(bytes: &[u8]) -> ParsedIdentity {
    if !is_object(bytes) {
        return ParsedIdentity::Invalid {
            detail: "atteso un oggetto JSON in posizione di documento".to_owned(),
        };
    }
    let Some(version) = schema_version(bytes) else {
        return ParsedIdentity::Invalid {
            detail: "intestazione schemaVersion non valida".to_owned(),
        };
    };
    if version != IDENTITY_SCHEMA_VERSION {
        return ParsedIdentity::UnsupportedSchema {
            schema_version: version,
        };
    }
    let document: IdentityV1 = match serde_json::from_slice(bytes) {
        Ok(document) => document,
        Err(error) => {
            return ParsedIdentity::Invalid {
                detail: error.to_string(),
            };
        }
    };
    if document.state == INITIAL_STATE {
        ParsedIdentity::Initial
    } else {
        ParsedIdentity::NonInitial {
            detail: format!("stato {:?} diverso da {INITIAL_STATE:?}", document.state),
        }
    }
}

pub(super) fn parse_controllers(bytes: &[u8]) -> ParsedControllers {
    if !is_object(bytes) {
        return ParsedControllers::Invalid {
            detail: "atteso un oggetto JSON in posizione di documento".to_owned(),
        };
    }
    let Some(version) = schema_version(bytes) else {
        return ParsedControllers::Invalid {
            detail: "intestazione schemaVersion non valida".to_owned(),
        };
    };
    if version != CONTROLLERS_SCHEMA_VERSION {
        return ParsedControllers::UnsupportedSchema {
            schema_version: version,
        };
    }
    let document: ControllersV1 = match serde_json::from_slice(bytes) {
        Ok(document) => document,
        Err(error) => {
            return ParsedControllers::Invalid {
                detail: error.to_string(),
            };
        }
    };
    if document.grant_revision != 0 {
        return ParsedControllers::NonInitial {
            detail: format!("grantRevision {} diversa da 0", document.grant_revision),
        };
    }
    if !document.controllers.is_empty() {
        return ParsedControllers::NonInitial {
            detail: format!(
                "registro con {} voci: in D3 deve essere vuoto",
                document.controllers.len()
            ),
        };
    }
    ParsedControllers::Initial
}

/// Documento iniziale di `identity.json`.
#[derive(Serialize)]
struct OutgoingIdentity {
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
    state: &'static str,
}

/// Documento iniziale di `controllers.json`.
#[derive(Serialize)]
struct OutgoingControllers {
    #[serde(rename = "schemaVersion")]
    schema_version: u64,
    #[serde(rename = "grantRevision")]
    grant_revision: u64,
    controllers: [serde_json::Value; 0],
}

pub(super) fn serialize_identity() -> Result<Vec<u8>, super::IdentityError> {
    let document = OutgoingIdentity {
        schema_version: IDENTITY_SCHEMA_VERSION,
        state: INITIAL_STATE,
    };
    serde_json::to_vec(&document)
        .map(|mut bytes| {
            bytes.push(b'\n');
            bytes
        })
        .map_err(|error| super::IdentityError::Serialize {
            detail: error.to_string(),
        })
}

pub(super) fn serialize_controllers() -> Result<Vec<u8>, super::IdentityError> {
    let document = OutgoingControllers {
        schema_version: CONTROLLERS_SCHEMA_VERSION,
        grant_revision: 0,
        controllers: [],
    };
    serde_json::to_vec(&document)
        .map(|mut bytes| {
            bytes.push(b'\n');
            bytes
        })
        .map_err(|error| super::IdentityError::Serialize {
            detail: error.to_string(),
        })
}
