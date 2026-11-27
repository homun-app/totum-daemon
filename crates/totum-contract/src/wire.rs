//! Codifica e decodifica dei messaggi: JSON UTF-8, un documento per messaggio.
//!
//! La decodifica è difensiva: la lunghezza è limitata, il parsing avviene in
//! `catch_unwind`, i campi mancanti o malformati producono
//! [`DecodeError`] e nessun panico deve sfuggire. I campi non consumati dal
//! payload sono ignorati, così un'aggiunta minore non interrompe un lettore
//! precedente. Un tipo di messaggio sconosciuto, invece, è rifiutato.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::envelope::{ApiSurface, Envelope, MessageType};
use crate::error::{DecodeError, EncodeError};
use crate::ids::{CorrelationId, MessageId};
use crate::limits::MAX_MESSAGE_BYTES;
use crate::message::{AdapterMessage, AdminMessage, Body, Message};
use crate::version::Version;

/// Legge i campi del payload consumandoli. I campi non consumati sono ignorati.
pub struct PayloadReader {
    fields: Map<String, Value>,
}

impl PayloadReader {
    pub(crate) fn new(fields: Map<String, Value>) -> Self {
        Self { fields }
    }

    pub(crate) fn take<T: DeserializeOwned>(
        &mut self,
        field: &'static str,
    ) -> Result<T, DecodeError> {
        let value = self
            .fields
            .remove(field)
            .ok_or(DecodeError::MissingField { field })?;
        convert_field(field, value)
    }

    pub(crate) fn take_optional<T: DeserializeOwned>(
        &mut self,
        field: &'static str,
    ) -> Result<Option<T>, DecodeError> {
        match self.fields.remove(field) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => convert_field(field, value).map(Some),
        }
    }
}

/// Decodifica un messaggio completo delle superfici A o C.
pub fn decode(input: &[u8]) -> Result<Message, DecodeError> {
    if input.len() > MAX_MESSAGE_BYTES {
        return Err(DecodeError::TooLarge {
            limit: MAX_MESSAGE_BYTES,
        });
    }
    let parsed = catch(|| serde_json::from_slice::<Value>(input))
        .map_err(|reason| DecodeError::InvalidJson { reason })?;
    let value = parsed.map_err(|error| DecodeError::InvalidJson {
        reason: error.to_string(),
    })?;
    let Value::Object(mut fields) = value else {
        return Err(DecodeError::NotAnObject);
    };
    let envelope = read_envelope(&mut fields)?;
    let mut reader = PayloadReader::new(fields);
    let body = match envelope.surface {
        ApiSurface::Admin => Body::Admin(AdminMessage::decode_payload(
            envelope.message_type,
            &mut reader,
        )?),
        ApiSurface::Adapter => Body::Adapter(AdapterMessage::decode_payload(
            envelope.message_type,
            &mut reader,
        )?),
    };
    let message = Message { envelope, body };
    message.validate()?;
    Ok(message)
}

/// Codifica un messaggio completo delle superfici A o C.
pub fn encode(message: &Message) -> Result<String, EncodeError> {
    message.validate().map_err(EncodeError::InvalidMessage)?;

    let mut fields = match message.body.payload_value()? {
        Value::Object(fields) => fields,
        _ => {
            return Err(EncodeError::Serialization {
                reason: "message payload must be a JSON object".to_string(),
            });
        }
    };
    let envelope = &message.envelope;
    fields.insert(
        "surface".to_string(),
        Value::String(envelope.surface.wire_name().to_string()),
    );
    fields.insert(
        "version".to_string(),
        serde_json::to_value(envelope.version).map_err(serialization_error)?,
    );
    fields.insert(
        "id".to_string(),
        serde_json::to_value(&envelope.message_id).map_err(serialization_error)?,
    );
    fields.insert(
        "type".to_string(),
        Value::String(envelope.message_type.wire_name().to_string()),
    );
    if let Some(correlation_id) = &envelope.correlation_id {
        fields.insert(
            "correlationId".to_string(),
            serde_json::to_value(correlation_id).map_err(serialization_error)?,
        );
    }

    let text = catch(|| serde_json::to_string(&Value::Object(fields)))
        .map_err(|reason| EncodeError::Serialization { reason })?
        .map_err(|error| EncodeError::Serialization {
            reason: error.to_string(),
        })?;
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(EncodeError::TooLarge {
            limit: MAX_MESSAGE_BYTES,
            found: text.len(),
        });
    }
    Ok(text)
}

fn read_envelope(fields: &mut Map<String, Value>) -> Result<Envelope, DecodeError> {
    let surface_value: String = take_field(fields, "surface")?;
    let surface = ApiSurface::parse(&surface_value).ok_or(DecodeError::UnknownSurface {
        value: surface_value,
    })?;
    let version: Version = take_field(fields, "version")?;
    let message_id: MessageId = take_field(fields, "id")?;
    let type_value: String = take_field(fields, "type")?;
    let message_type =
        MessageType::parse(surface, &type_value).ok_or(DecodeError::UnknownMessageType {
            surface,
            message_type: type_value,
        })?;
    let correlation_id: Option<CorrelationId> = take_optional_field(fields, "correlationId")?;

    if message_type.requires_correlation() && correlation_id.is_none() {
        return Err(DecodeError::MissingField {
            field: "correlationId",
        });
    }
    if !message_type.requires_correlation() && correlation_id.is_some() {
        return Err(DecodeError::InvalidField {
            field: "correlationId".to_string(),
            reason: "is only used by responses".to_string(),
        });
    }

    let envelope = Envelope {
        surface,
        version,
        message_id,
        message_type,
        correlation_id,
    };
    envelope.check_supported()?;
    Ok(envelope)
}

fn take_field<T: DeserializeOwned>(
    fields: &mut Map<String, Value>,
    field: &'static str,
) -> Result<T, DecodeError> {
    let value = fields
        .remove(field)
        .ok_or(DecodeError::MissingField { field })?;
    convert_field(field, value)
}

fn take_optional_field<T: DeserializeOwned>(
    fields: &mut Map<String, Value>,
    field: &'static str,
) -> Result<Option<T>, DecodeError> {
    match fields.remove(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => convert_field(field, value).map(Some),
    }
}

fn convert_field<T: DeserializeOwned>(field: &'static str, value: Value) -> Result<T, DecodeError> {
    let decoded = catch(|| serde_json::from_value::<T>(value)).map_err(|reason| {
        DecodeError::InvalidField {
            field: field.to_string(),
            reason,
        }
    })?;
    decoded.map_err(|error| DecodeError::InvalidField {
        field: field.to_string(),
        reason: error.to_string(),
    })
}

fn serialization_error(error: serde_json::Error) -> EncodeError {
    EncodeError::Serialization {
        reason: error.to_string(),
    }
}

fn catch<T>(operation: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(operation)).map_err(|panic| panic_message(&panic))
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic while decoding".to_string()
    }
}
