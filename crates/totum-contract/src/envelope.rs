//! Envelope comune, superfici e tipi di messaggio.

use crate::error::DecodeError;
use crate::ids::{CorrelationId, MessageId};
use crate::version::{ADAPTER_SUPPORTED_RANGE, ADMIN_SUPPORTED_RANGE, ProtocolRange, Version};

/// Superficie tecnica del contratto. Non è il prodotto né la superficie
/// tecnica dell'adapter: quelle sono identificatori nel payload della
/// superficie C.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApiSurface {
    /// Superficie A, amministrazione locale.
    Admin,
    /// Superficie C, contratto adapter–hub.
    Adapter,
}

impl ApiSurface {
    /// Nome wire della superficie.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Adapter => "adapter",
        }
    }

    /// Interpreta il nome wire della superficie.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "adapter" => Some(Self::Adapter),
            _ => None,
        }
    }

    /// Intervallo di versioni supportate da questo crate per la superficie.
    pub const fn supported_range(self) -> ProtocolRange {
        match self {
            Self::Admin => ADMIN_SUPPORTED_RANGE,
            Self::Adapter => ADAPTER_SUPPORTED_RANGE,
        }
    }
}

/// Tipo di messaggio. I nomi coincidono con il campo `type` dell'envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MessageType {
    GetStatus,
    StatusReport,
    GetHubIdentity,
    HubIdentityReport,
    AdapterHello,
    HubHello,
    Discover,
    DiscoveryResult,
    Attach,
    Detach,
    SessionEvent,
    Operation,
    OperationAccepted,
    OperationRejected,
    OperationResult,
    Health,
    Ping,
    Pong,
    Shutdown,
    Error,
}

impl MessageType {
    /// Nome wire del tipo di messaggio.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::GetStatus => "getStatus",
            Self::StatusReport => "statusReport",
            Self::GetHubIdentity => "getHubIdentity",
            Self::HubIdentityReport => "hubIdentityReport",
            Self::AdapterHello => "adapterHello",
            Self::HubHello => "hubHello",
            Self::Discover => "discover",
            Self::DiscoveryResult => "discoveryResult",
            Self::Attach => "attach",
            Self::Detach => "detach",
            Self::SessionEvent => "sessionEvent",
            Self::Operation => "operation",
            Self::OperationAccepted => "operationAccepted",
            Self::OperationRejected => "operationRejected",
            Self::OperationResult => "operationResult",
            Self::Health => "health",
            Self::Ping => "ping",
            Self::Pong => "pong",
            Self::Shutdown => "shutdown",
            Self::Error => "error",
        }
    }

    /// Superficie a cui appartiene il tipo.
    pub const fn surface(self) -> ApiSurface {
        match self {
            Self::GetStatus
            | Self::StatusReport
            | Self::GetHubIdentity
            | Self::HubIdentityReport => ApiSurface::Admin,
            _ => ApiSurface::Adapter,
        }
    }

    /// Interpreta il tipo sulla superficie dichiarata. Lo stesso nome su
    /// un'altra superficie non è un tipo noto.
    pub fn parse(surface: ApiSurface, value: &str) -> Option<Self> {
        let message_type = match value {
            "getStatus" => Self::GetStatus,
            "statusReport" => Self::StatusReport,
            "getHubIdentity" => Self::GetHubIdentity,
            "hubIdentityReport" => Self::HubIdentityReport,
            "adapterHello" => Self::AdapterHello,
            "hubHello" => Self::HubHello,
            "discover" => Self::Discover,
            "discoveryResult" => Self::DiscoveryResult,
            "attach" => Self::Attach,
            "detach" => Self::Detach,
            "sessionEvent" => Self::SessionEvent,
            "operation" => Self::Operation,
            "operationAccepted" => Self::OperationAccepted,
            "operationRejected" => Self::OperationRejected,
            "operationResult" => Self::OperationResult,
            "health" => Self::Health,
            "ping" => Self::Ping,
            "pong" => Self::Pong,
            "shutdown" => Self::Shutdown,
            "error" => Self::Error,
            _ => return None,
        };
        if message_type.surface() == surface {
            Some(message_type)
        } else {
            None
        }
    }

    /// Vero se la risposta deve portare l'identificatore del messaggio a cui
    /// risponde. Le risposte della superficie C si correlano con gli
    /// identificatori di discovery e di operazione.
    pub const fn requires_correlation(self) -> bool {
        matches!(self, Self::StatusReport | Self::HubIdentityReport)
    }
}

/// Campi comuni a ogni messaggio delle superfici A e C.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub surface: ApiSurface,
    pub version: Version,
    pub message_id: MessageId,
    pub message_type: MessageType,
    pub correlation_id: Option<CorrelationId>,
}

impl Envelope {
    /// Costruisce un envelope senza correlazione.
    pub fn new(
        surface: ApiSurface,
        version: Version,
        message_id: MessageId,
        message_type: MessageType,
    ) -> Self {
        Self {
            surface,
            version,
            message_id,
            message_type,
            correlation_id: None,
        }
    }

    /// Vero se la versione dell'envelope è supportata dalla superficie.
    pub const fn is_supported(&self) -> bool {
        self.version.is_valid()
            && self.version.major() == self.surface.supported_range().min().major()
    }

    /// Verifica che la versione dell'envelope sia supportata dalla superficie.
    /// Una maggiore diversa non è mai compatibile.
    pub fn check_supported(&self) -> Result<(), DecodeError> {
        if self.is_supported() {
            Ok(())
        } else {
            Err(DecodeError::IncompatibleVersion {
                found: self.version,
                supported: self.surface.supported_range(),
            })
        }
    }
}
