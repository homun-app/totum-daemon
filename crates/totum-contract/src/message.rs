//! Messaggi tipizzati delle superfici A e C e dispatcher per tipo.

use serde::Serialize;
use serde_json::Value;

use crate::envelope::{ApiSurface, Envelope, MessageType};
use crate::error::{DecodeError, EncodeError, ValidationError};
use crate::surface_a::{GetHubIdentity, GetStatus, HubIdentityReport, StatusReport};
use crate::surface_c::{
    AdapterHello, Attach, Detach, Discover, DiscoveryResult, ErrorMessage, Health, HubHello,
    Operation, OperationAccepted, OperationRejected, OperationResult, Ping, Pong, SessionEvent,
    Shutdown,
};
use crate::wire::PayloadReader;

/// Corpo tipizzato di un messaggio della superficie A.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminMessage {
    GetStatus(GetStatus),
    StatusReport(StatusReport),
    GetHubIdentity(GetHubIdentity),
    HubIdentityReport(HubIdentityReport),
}

impl AdminMessage {
    /// Tipo di messaggio corrispondente al corpo.
    pub fn message_type(&self) -> MessageType {
        match self {
            Self::GetStatus(_) => MessageType::GetStatus,
            Self::StatusReport(_) => MessageType::StatusReport,
            Self::GetHubIdentity(_) => MessageType::GetHubIdentity,
            Self::HubIdentityReport(_) => MessageType::HubIdentityReport,
        }
    }

    pub(crate) fn decode_payload(
        message_type: MessageType,
        reader: &mut PayloadReader,
    ) -> Result<Self, DecodeError> {
        match message_type {
            MessageType::GetStatus => Ok(Self::GetStatus(GetStatus::from_payload(reader)?)),
            MessageType::StatusReport => {
                Ok(Self::StatusReport(StatusReport::from_payload(reader)?))
            }
            MessageType::GetHubIdentity => {
                Ok(Self::GetHubIdentity(GetHubIdentity::from_payload(reader)?))
            }
            MessageType::HubIdentityReport => Ok(Self::HubIdentityReport(
                HubIdentityReport::from_payload(reader)?,
            )),
            other => Err(DecodeError::UnknownMessageType {
                surface: ApiSurface::Admin,
                message_type: other.wire_name().to_string(),
            }),
        }
    }

    /// Applica le regole di contenuto del corpo.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::GetStatus(body) => body.validate(),
            Self::StatusReport(body) => body.validate(),
            Self::GetHubIdentity(body) => body.validate(),
            Self::HubIdentityReport(body) => body.validate(),
        }
    }

    fn payload_value(&self) -> Result<Value, EncodeError> {
        match self {
            Self::GetStatus(body) => to_value(body),
            Self::StatusReport(body) => to_value(body),
            Self::GetHubIdentity(body) => to_value(body),
            Self::HubIdentityReport(body) => to_value(body),
        }
    }
}

/// Corpo tipizzato di un messaggio della superficie C.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterMessage {
    AdapterHello(AdapterHello),
    HubHello(HubHello),
    Discover(Discover),
    DiscoveryResult(DiscoveryResult),
    Attach(Attach),
    Detach(Detach),
    SessionEvent(SessionEvent),
    Operation(Operation),
    OperationAccepted(OperationAccepted),
    OperationRejected(OperationRejected),
    OperationResult(OperationResult),
    Health(Health),
    Ping(Ping),
    Pong(Pong),
    Shutdown(Shutdown),
    Error(ErrorMessage),
}

impl AdapterMessage {
    /// Tipo di messaggio corrispondente al corpo.
    pub fn message_type(&self) -> MessageType {
        match self {
            Self::AdapterHello(_) => MessageType::AdapterHello,
            Self::HubHello(_) => MessageType::HubHello,
            Self::Discover(_) => MessageType::Discover,
            Self::DiscoveryResult(_) => MessageType::DiscoveryResult,
            Self::Attach(_) => MessageType::Attach,
            Self::Detach(_) => MessageType::Detach,
            Self::SessionEvent(_) => MessageType::SessionEvent,
            Self::Operation(_) => MessageType::Operation,
            Self::OperationAccepted(_) => MessageType::OperationAccepted,
            Self::OperationRejected(_) => MessageType::OperationRejected,
            Self::OperationResult(_) => MessageType::OperationResult,
            Self::Health(_) => MessageType::Health,
            Self::Ping(_) => MessageType::Ping,
            Self::Pong(_) => MessageType::Pong,
            Self::Shutdown(_) => MessageType::Shutdown,
            Self::Error(_) => MessageType::Error,
        }
    }

    pub(crate) fn decode_payload(
        message_type: MessageType,
        reader: &mut PayloadReader,
    ) -> Result<Self, DecodeError> {
        match message_type {
            MessageType::AdapterHello => {
                Ok(Self::AdapterHello(AdapterHello::from_payload(reader)?))
            }
            MessageType::HubHello => Ok(Self::HubHello(HubHello::from_payload(reader)?)),
            MessageType::Discover => Ok(Self::Discover(Discover::from_payload(reader)?)),
            MessageType::DiscoveryResult => Ok(Self::DiscoveryResult(
                DiscoveryResult::from_payload(reader)?,
            )),
            MessageType::Attach => Ok(Self::Attach(Attach::from_payload(reader)?)),
            MessageType::Detach => Ok(Self::Detach(Detach::from_payload(reader)?)),
            MessageType::SessionEvent => {
                Ok(Self::SessionEvent(SessionEvent::from_payload(reader)?))
            }
            MessageType::Operation => Ok(Self::Operation(Operation::from_payload(reader)?)),
            MessageType::OperationAccepted => Ok(Self::OperationAccepted(
                OperationAccepted::from_payload(reader)?,
            )),
            MessageType::OperationRejected => Ok(Self::OperationRejected(
                OperationRejected::from_payload(reader)?,
            )),
            MessageType::OperationResult => Ok(Self::OperationResult(
                OperationResult::from_payload(reader)?,
            )),
            MessageType::Health => Ok(Self::Health(Health::from_payload(reader)?)),
            MessageType::Ping => Ok(Self::Ping(Ping::from_payload(reader)?)),
            MessageType::Pong => Ok(Self::Pong(Pong::from_payload(reader)?)),
            MessageType::Shutdown => Ok(Self::Shutdown(Shutdown::from_payload(reader)?)),
            MessageType::Error => Ok(Self::Error(ErrorMessage::from_payload(reader)?)),
            other => Err(DecodeError::UnknownMessageType {
                surface: ApiSurface::Adapter,
                message_type: other.wire_name().to_string(),
            }),
        }
    }

    /// Applica le regole di contenuto del corpo.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::AdapterHello(body) => body.validate(),
            Self::HubHello(body) => body.validate(),
            Self::Discover(body) => body.validate(),
            Self::DiscoveryResult(body) => body.validate(),
            Self::Attach(body) => body.validate(),
            Self::Detach(body) => body.validate(),
            Self::SessionEvent(body) => body.validate(),
            Self::Operation(body) => body.validate(),
            Self::OperationAccepted(body) => body.validate(),
            Self::OperationRejected(body) => body.validate(),
            Self::OperationResult(body) => body.validate(),
            Self::Health(body) => body.validate(),
            Self::Ping(body) => body.validate(),
            Self::Pong(body) => body.validate(),
            Self::Shutdown(body) => body.validate(),
            Self::Error(body) => body.validate(),
        }
    }

    fn payload_value(&self) -> Result<Value, EncodeError> {
        match self {
            Self::AdapterHello(body) => to_value(body),
            Self::HubHello(body) => to_value(body),
            Self::Discover(body) => to_value(body),
            Self::DiscoveryResult(body) => to_value(body),
            Self::Attach(body) => to_value(body),
            Self::Detach(body) => to_value(body),
            Self::SessionEvent(body) => to_value(body),
            Self::Operation(body) => to_value(body),
            Self::OperationAccepted(body) => to_value(body),
            Self::OperationRejected(body) => to_value(body),
            Self::OperationResult(body) => to_value(body),
            Self::Health(body) => to_value(body),
            Self::Ping(body) => to_value(body),
            Self::Pong(body) => to_value(body),
            Self::Shutdown(body) => to_value(body),
            Self::Error(body) => to_value(body),
        }
    }
}

/// Corpo di un messaggio, sulla superficie che lo contiene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Admin(AdminMessage),
    Adapter(AdapterMessage),
}

impl Body {
    /// Superficie del corpo.
    pub fn surface(&self) -> ApiSurface {
        match self {
            Self::Admin(_) => ApiSurface::Admin,
            Self::Adapter(_) => ApiSurface::Adapter,
        }
    }

    /// Tipo di messaggio del corpo.
    pub fn message_type(&self) -> MessageType {
        match self {
            Self::Admin(message) => message.message_type(),
            Self::Adapter(message) => message.message_type(),
        }
    }

    /// Applica le regole di contenuto del corpo.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Admin(message) => message.validate(),
            Self::Adapter(message) => message.validate(),
        }
    }

    pub(crate) fn payload_value(&self) -> Result<Value, EncodeError> {
        match self {
            Self::Admin(message) => message.payload_value(),
            Self::Adapter(message) => message.payload_value(),
        }
    }
}

/// Envelope e corpo di un messaggio decodificato o da codificare.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub envelope: Envelope,
    pub body: Body,
}

impl Message {
    /// Superficie del messaggio.
    pub fn surface(&self) -> ApiSurface {
        self.body.surface()
    }

    /// Tipo del messaggio.
    pub fn message_type(&self) -> MessageType {
        self.body.message_type()
    }

    /// Verifica gli invarianti del messaggio, non soltanto del corpo:
    /// l'envelope dichiara la superficie e il tipo del corpo, la versione è
    /// supportata dalla superficie, `correlationId` compare soltanto nelle
    /// risposte, e il corpo rispetta limiti e riferimenti interni.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.envelope.surface != self.body.surface() {
            return Err(ValidationError::invalid(
                "surface",
                "does not match the message type",
            ));
        }
        if self.envelope.message_type != self.body.message_type() {
            return Err(ValidationError::invalid(
                "type",
                "does not match the message body",
            ));
        }
        if !self.envelope.is_supported() {
            return Err(ValidationError::invalid(
                "version",
                "is not supported by the surface",
            ));
        }
        match (
            &self.envelope.correlation_id,
            self.envelope.message_type.requires_correlation(),
        ) {
            (None, true) => {
                return Err(ValidationError::invalid(
                    "correlationId",
                    "is required by a response",
                ));
            }
            (Some(_), false) => {
                return Err(ValidationError::invalid(
                    "correlationId",
                    "is only used by responses",
                ));
            }
            _ => {}
        }
        self.body.validate()
    }
}

fn to_value<T: Serialize>(body: &T) -> Result<Value, EncodeError> {
    serde_json::to_value(body).map_err(|error| EncodeError::Serialization {
        reason: error.to_string(),
    })
}
