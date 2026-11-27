//! Errori distinti per fase: decodifica locale, codifica, validazione e
//! errori applicativi che viaggiano come messaggio.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::envelope::ApiSurface;
use crate::limits;
use crate::version::{ProtocolRange, Version};

/// Errore di decodifica di un messaggio esterno. Non è un errore applicativo:
/// descrive perché i byte non diventano un messaggio tipizzato.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Il messaggio supera il limite di lavoro in byte.
    TooLarge { limit: usize },
    /// I byte non sono JSON valido.
    InvalidJson { reason: String },
    /// L'envelope non è un oggetto JSON.
    NotAnObject,
    /// Manca un campo obbligatorio dell'envelope o del payload.
    MissingField { field: &'static str },
    /// Un campo ha tipo o formato non valido.
    InvalidField { field: String, reason: String },
    /// La superficie dichiarata non è riconosciuta.
    UnknownSurface { value: String },
    /// Il tipo di messaggio non è riconosciuto sulla superficie dichiarata.
    UnknownMessageType {
        surface: ApiSurface,
        message_type: String,
    },
    /// La versione dichiarata nell'envelope non è compatibile con la superficie.
    IncompatibleVersion {
        found: Version,
        supported: ProtocolRange,
    },
    /// Una collezione o un testo supera il limite di lavoro.
    LimitExceeded {
        field: String,
        limit: usize,
        found: usize,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => {
                write!(formatter, "message exceeds the {limit}-byte working limit")
            }
            Self::InvalidJson { reason } => write!(formatter, "invalid JSON: {reason}"),
            Self::NotAnObject => formatter.write_str("the message envelope must be a JSON object"),
            Self::MissingField { field } => {
                write!(formatter, "missing required field `{field}`")
            }
            Self::InvalidField { field, reason } => {
                write!(formatter, "invalid field `{field}`: {reason}")
            }
            Self::UnknownSurface { value } => write!(formatter, "unknown API surface `{value}`"),
            Self::UnknownMessageType {
                surface,
                message_type,
            } => write!(
                formatter,
                "unknown {} message type `{message_type}`",
                surface.wire_name()
            ),
            Self::IncompatibleVersion { found, supported } => write!(
                formatter,
                "message version {found} is outside the supported range {supported}"
            ),
            Self::LimitExceeded {
                field,
                limit,
                found,
            } => write!(
                formatter,
                "field `{field}` has {found} elements, the working limit is {limit}"
            ),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Errore di validazione di un messaggio tipizzato, usato anche in codifica.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationError {
    /// Un campo ha contenuto non valido.
    InvalidField { field: String, reason: String },
    /// Una collezione o un testo supera il limite di lavoro.
    LimitExceeded {
        field: String,
        limit: usize,
        found: usize,
    },
}

impl ValidationError {
    pub(crate) fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidField {
            field: field.into(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField { field, reason } => {
                write!(formatter, "invalid field `{field}`: {reason}")
            }
            Self::LimitExceeded {
                field,
                limit,
                found,
            } => write!(
                formatter,
                "field `{field}` has {found} elements, the working limit is {limit}"
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

impl From<ValidationError> for ProtocolError {
    fn from(error: ValidationError) -> Self {
        Self::invalid_payload(error.to_string())
    }
}

impl From<ValidationError> for DecodeError {
    fn from(error: ValidationError) -> Self {
        match error {
            ValidationError::InvalidField { field, reason } => {
                DecodeError::InvalidField { field, reason }
            }
            ValidationError::LimitExceeded {
                field,
                limit,
                found,
            } => DecodeError::LimitExceeded {
                field,
                limit,
                found,
            },
        }
    }
}

/// Errore di codifica di un messaggio tipizzato.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// Il messaggio non rispetta il contratto.
    InvalidMessage(ValidationError),
    /// Il testo codificato supera il limite di lavoro in byte.
    TooLarge { limit: usize, found: usize },
    /// La serializzazione JSON è fallita.
    Serialization { reason: String },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMessage(error) => write!(formatter, "invalid message: {error}"),
            Self::TooLarge { limit, found } => write!(
                formatter,
                "encoded message has {found} bytes, the working limit is {limit}"
            ),
            Self::Serialization { reason } => write!(formatter, "serialization failed: {reason}"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Codici degli errori applicativi trasportati nel messaggio `error` o in un
/// rifiuto di operazione. L'esito incerto ha una rappresentazione distinta,
/// [`crate::surface_c::OperationOutcome::OutcomeUnknown`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    ProtocolIncompatible,
    InvalidPayload,
    UnsupportedCapability,
    TargetUnavailable,
    StaleRevision,
    RequestExpired,
    AlreadyResolved,
    RateLimited,
    AdapterFailure,
}

/// Errore applicativo strutturato. `detail` è un dettaglio redatto dal
/// mittente e limitato in lunghezza.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ProtocolError {
    /// Crea un errore applicativo.
    pub fn new(code: ErrorCode, retryable: bool) -> Self {
        Self {
            code,
            retryable,
            detail: None,
        }
    }

    /// Aggiunge un dettaglio redatto.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Errore di versione incompatibile.
    pub fn protocol_incompatible(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::ProtocolIncompatible, false).with_detail(detail)
    }

    /// Errore di payload non valido.
    pub fn invalid_payload(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidPayload, false).with_detail(detail)
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if let Some(detail) = &self.detail {
            limits::validate_detail("detail", detail)?;
        }
        Ok(())
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?} (retryable: {})", self.code, self.retryable)?;
        if let Some(detail) = &self.detail {
            write!(formatter, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProtocolError {}
