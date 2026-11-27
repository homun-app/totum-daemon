//! Superficie A: amministrazione locale.
//!
//! Solo richiesta e risposta di stato e identità dell'hub. L'identità è
//! presente oppure non ancora inizializzata: non esiste un valore finto di
//! impronta per la prima esecuzione.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{DecodeError, ValidationError};
use crate::ids::InvalidValue;
use crate::limits;
use crate::wire::PayloadReader;

/// Richiesta di stato. Non ha campi.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetStatus {}

impl GetStatus {
    pub(crate) fn from_payload(_reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {})
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Risposta di stato dell'hub.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusReport {
    /// Versione del prodotto hub, distinta dalla versione della superficie.
    pub hub_version: String,
    /// Identità dell'hub: presente oppure non ancora inizializzata.
    pub identity: IdentityState,
}

impl StatusReport {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            hub_version: reader.take("hubVersion")?,
            identity: reader.take("identity")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        limits::validate_version_label("hubVersion", &self.hub_version)
    }
}

/// Richiesta dell'impronta dell'hub. Non ha campi.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct GetHubIdentity {}

impl GetHubIdentity {
    pub(crate) fn from_payload(_reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {})
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Risposta con l'identità dell'hub.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubIdentityReport {
    pub identity: IdentityState,
}

impl HubIdentityReport {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            identity: reader.take("identity")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Stato dell'identità dell'hub.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum IdentityState {
    /// Prima esecuzione: nessuna identità è stata ancora creata.
    NotInitialized,
    /// Identità presente, con la sua impronta.
    Present { fingerprint: Fingerprint },
}

/// Impronta dell'identità dell'hub: stringa esadecimale di lunghezza pari.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Fingerprint(String);

impl Fingerprint {
    /// Crea un'impronta se rispetta il formato di lavoro.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidValue> {
        let value = value.into();
        if value.len() < 16 {
            return Err(InvalidValue {
                field: "Fingerprint",
                reason: "must contain at least 16 hexadecimal characters",
            });
        }
        if value.len() > limits::MAX_FINGERPRINT_BYTES {
            return Err(InvalidValue {
                field: "Fingerprint",
                reason: "is longer than the working limit",
            });
        }
        if value.len() % 2 != 0 {
            return Err(InvalidValue {
                field: "Fingerprint",
                reason: "must contain an even number of hexadecimal characters",
            });
        }
        if !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(InvalidValue {
                field: "Fingerprint",
                reason: "contains characters outside [0-9a-fA-F]",
            });
        }
        Ok(Self(value))
    }

    /// Restituisce l'impronta come stringa.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}
