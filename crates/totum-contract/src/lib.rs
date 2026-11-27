//! Contratti wire di lavoro per le superfici A (amministrazione locale) e
//! C (adapter–hub) del daemon Totum.
//!
//! Questo crate è deliberatamente minimo e non è un protocollo stabile:
//! contiene envelope, versioni e negoziazione, i messaggi delle superfici A e
//! C, gli errori strutturati, la codifica JSON e le regole di validazione. Non
//! contiene trasporti, socket, listener, hub, registry, journal, autorizzazione
//! operativa, pairing, crittografia o adapter.
//!
//! La codifica scelta per i contratti di lavoro A/C è JSON UTF-8, un documento
//! per messaggio. La scelta non fissa la codifica del futuro protocollo mobile
//! né quella del relay.

#![forbid(unsafe_code)]

pub mod envelope;
pub mod error;
pub mod ids;
pub mod limits;
pub mod message;
pub mod surface_a;
pub mod surface_c;
pub mod version;
pub mod wire;

pub use envelope::{ApiSurface, Envelope, MessageType};
pub use error::{DecodeError, EncodeError, ErrorCode, ProtocolError, ValidationError};
pub use ids::{
    AdapterInstanceId, CapabilityId, ConversationId, CorrelationId, DiscoveryId, ExecutionId,
    InvalidValue, MessageId, OperationId, ProductId, ProjectId, RequestId, RuntimeInstanceId,
    SessionId, TechnicalSurfaceId,
};
pub use limits::MAX_MESSAGE_BYTES;
pub use message::{AdapterMessage, AdminMessage, Body, Message};
pub use surface_a::{
    Fingerprint, GetHubIdentity, GetStatus, HubIdentityReport, IdentityState, StatusReport,
};
pub use surface_c::{
    AdapterHello, Attach, CompletionReason, ConversationRef, Detach, Discover, DiscoveryResult,
    ErrorMessage, ExecutionRef, Health, HealthState, HubHello, KNOWN_CAPABILITIES, Operation,
    OperationAccepted, OperationBody, OperationOutcome, OperationRejected, OperationResult, Ping,
    Pong, ProjectDescriptor, RuntimeInstance, RuntimeState, SessionDescriptor, SessionEvent,
    SessionEventKind, SessionMode, SessionState, Shutdown, ShutdownReason, UnknownOutcomeReason,
    negotiate_adapter_handshake,
};
pub use version::{
    ADAPTER_SUPPORTED_RANGE, ADAPTER_SURFACE_VERSION, ADMIN_SUPPORTED_RANGE, ADMIN_SURFACE_VERSION,
    NegotiationError, ProtocolRange, Version, negotiate,
};
pub use wire::{decode, encode};
