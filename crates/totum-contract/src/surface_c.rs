//! Superficie C: contratto adapter–hub.
//!
//! La superficie non trasporta identità né grant dei controller e un messaggio
//! `operation` è un'azione strutturata, non un comando shell generico.
//!
//! Distinzioni rappresentate:
//!
//! - prodotto (`ProductId`) e superficie tecnica (`TechnicalSurfaceId`);
//! - istanza runtime (`RuntimeInstanceId`), conversazione persistita
//!   (`ConversationId`) ed esecuzione viva (`ExecutionId`);
//! - capability dichiarata dall'adapter e capability riconosciuta dall'hub;
//!   l'autorizzazione dell'hub non viaggia su questa superficie;
//! - accettazione di un'operazione (`operationAccepted`) e suo esito
//!   (`operationResult`);
//! - errore certo (`OperationOutcome::Failed`) ed esito incerto
//!   (`OperationOutcome::OutcomeUnknown`).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::{DecodeError, ProtocolError, ValidationError};
use crate::ids::{
    AdapterInstanceId, CapabilityId, ConversationId, DiscoveryId, ExecutionId, OperationId,
    ProductId, ProjectId, RequestId, RuntimeInstanceId, SessionId, TechnicalSurfaceId,
};
use crate::limits;
use crate::version::{ProtocolRange, Version};
use crate::wire::PayloadReader;

/// Capability riconosciute da questa revisione dei contratti di lavoro.
pub const KNOWN_CAPABILITIES: &[&str] = &[
    "discoverSessions",
    "readStatus",
    "readTimeline",
    "readFiles",
    "readDiff",
    "receiveApproval",
    "decideApproval",
    "receiveInput",
    "reply",
    "startTask",
    "pause",
    "resume",
    "stop",
    "retry",
];

impl CapabilityId {
    /// Vero se la capability è riconosciuta da questa revisione. Una
    /// capability non riconosciuta resta dichiarata ma non è negoziata.
    pub fn is_known(&self) -> bool {
        KNOWN_CAPABILITIES.contains(&self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeState {
    Available,
    Degraded,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionMode {
    Observed,
    Prepared,
    Managed,
    Ended,
}

/// I sette stati visivi del riferimento di design. Lo stato è indipendente dal
/// marchio dell'agente e non è codificato dal solo colore.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionState {
    Approval,
    Error,
    Input,
    Working,
    Completed,
    Paused,
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompletionReason {
    Completed,
    Cancelled,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HealthState {
    Healthy,
    Degraded,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShutdownReason {
    Requested,
    Restarting,
    Fatal,
}

/// Messaggio di handshake dell'adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterHello {
    pub adapter_instance_id: AdapterInstanceId,
    pub product_id: ProductId,
    pub technical_surface_id: TechnicalSurfaceId,
    pub adapter_version: String,
    pub supported_protocol: ProtocolRange,
    pub declared_capabilities: Vec<CapabilityId>,
}

impl AdapterHello {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            adapter_instance_id: reader.take("adapterInstanceId")?,
            product_id: reader.take("productId")?,
            technical_surface_id: reader.take("technicalSurfaceId")?,
            adapter_version: reader.take("adapterVersion")?,
            supported_protocol: reader.take("supportedProtocol")?,
            declared_capabilities: reader.take("declaredCapabilities")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        limits::validate_version_label("adapterVersion", &self.adapter_version)?;
        if !self.supported_protocol.is_valid() {
            return Err(ValidationError::invalid(
                "supportedProtocol",
                "must cover one major version with min <= max",
            ));
        }
        validate_capabilities("declaredCapabilities", &self.declared_capabilities)
    }

    /// Verifica che `hubHello` rispetti l'intervallo dell'adapter e selezioni
    /// la versione più alta in comune. Una versione maggiore diversa, o una
    /// selezione non negoziabile, produce `protocolIncompatible`. Una
    /// capability riconosciuta deve essere stata dichiarata e appartenere a
    /// questa revisione; il riconoscimento non è un'autorizzazione.
    pub fn verify_hub_hello(&self, hub: &HubHello) -> Result<(), ProtocolError> {
        self.validate()?;
        hub.validate()?;
        if !self.supported_protocol.is_valid() || !hub.hub_supported.is_valid() {
            return Err(ProtocolError::invalid_payload(
                "protocol ranges must cover one major version",
            ));
        }
        let expected = self
            .supported_protocol
            .negotiate(hub.hub_supported)
            .map_err(ProtocolError::from)?;
        if hub.selected_version != expected {
            return Err(ProtocolError::protocol_incompatible(format!(
                "hub selected {} but the highest common version is {}",
                hub.selected_version, expected
            )));
        }
        if !self.supported_protocol.contains(hub.selected_version)
            || !hub.hub_supported.contains(hub.selected_version)
        {
            return Err(ProtocolError::protocol_incompatible(
                "hub selected a version outside a supported range",
            ));
        }
        for capability in &hub.recognized_capabilities {
            if !self.declared_capabilities.contains(capability) {
                return Err(ProtocolError::invalid_payload(
                    "hub recognized a capability the adapter did not declare",
                ));
            }
        }
        Ok(())
    }
}

/// Risposta di handshake dell'hub.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubHello {
    pub selected_version: Version,
    pub hub_supported: ProtocolRange,
    /// Capability riconosciute per questa connessione: solo quelle note a
    /// questa revisione, dichiarate dall'adapter. Non sono un grant:
    /// l'autorizzazione dell'hub non viaggia sulla superficie C.
    pub recognized_capabilities: Vec<CapabilityId>,
}

impl HubHello {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            selected_version: reader.take("selectedVersion")?,
            hub_supported: reader.take("hubSupported")?,
            recognized_capabilities: reader.take("recognizedCapabilities")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if !self.hub_supported.is_valid() {
            return Err(ValidationError::invalid(
                "hubSupported",
                "must cover one major version with min <= max",
            ));
        }
        if !self.hub_supported.contains(self.selected_version) {
            return Err(ValidationError::invalid(
                "selectedVersion",
                "must fall inside hubSupported",
            ));
        }
        validate_capabilities("recognizedCapabilities", &self.recognized_capabilities)?;
        for capability in &self.recognized_capabilities {
            if !capability.is_known() {
                return Err(ValidationError::invalid(
                    "recognizedCapabilities",
                    "must contain only capabilities recognized by this revision",
                ));
            }
        }
        Ok(())
    }
}

/// Negozia l'handshake lato hub e restituisce `hubHello`.
pub fn negotiate_adapter_handshake(
    hello: &AdapterHello,
    hub_supported: ProtocolRange,
) -> Result<HubHello, ProtocolError> {
    hello.validate()?;
    let selected_version = hub_supported
        .negotiate(hello.supported_protocol)
        .map_err(ProtocolError::from)?;
    let recognized_capabilities = hello
        .declared_capabilities
        .iter()
        .filter(|capability| capability.is_known())
        .cloned()
        .collect();
    let hub_hello = HubHello {
        selected_version,
        hub_supported,
        recognized_capabilities,
    };
    hub_hello.validate()?;
    Ok(hub_hello)
}

/// Richiesta di discovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discover {
    pub discovery_id: DiscoveryId,
}

impl Discover {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            discovery_id: reader.take("discoveryId")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Istanza runtime dichiarata dall'adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInstance {
    pub runtime_id: RuntimeInstanceId,
    pub product_id: ProductId,
    pub technical_surface_id: TechnicalSurfaceId,
    pub runtime_version: String,
    pub state: RuntimeState,
}

/// Progetto nel perimetro dichiarato.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDescriptor {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Riferimento alla conversazione persistita nel fornitore.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRef {
    pub conversation_id: ConversationId,
}

/// Riferimento all'esecuzione viva. Due esecuzioni della stessa conversazione
/// sono due sessioni.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionRef {
    pub execution_id: ExecutionId,
}

/// Sessione dichiarata nella discovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDescriptor {
    pub session_id: SessionId,
    pub project_id: ProjectId,
    pub runtime_id: RuntimeInstanceId,
    pub mode: SessionMode,
    pub state: SessionState,
    pub conversation: ConversationRef,
    pub execution: ExecutionRef,
    pub declared_capabilities: Vec<CapabilityId>,
}

/// Risultato della discovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryResult {
    pub discovery_id: DiscoveryId,
    pub runtimes: Vec<RuntimeInstance>,
    pub projects: Vec<ProjectDescriptor>,
    pub sessions: Vec<SessionDescriptor>,
}

impl DiscoveryResult {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            discovery_id: reader.take("discoveryId")?,
            runtimes: reader.take("runtimes")?,
            projects: reader.take("projects")?,
            sessions: reader.take("sessions")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        limits::check_len(
            "runtimes",
            self.runtimes.len(),
            limits::MAX_DISCOVERED_RUNTIMES,
        )?;
        limits::check_len(
            "projects",
            self.projects.len(),
            limits::MAX_DISCOVERED_PROJECTS,
        )?;
        limits::check_len(
            "sessions",
            self.sessions.len(),
            limits::MAX_DISCOVERED_SESSIONS,
        )?;

        let mut runtime_ids: HashSet<&str> = HashSet::new();
        for runtime in &self.runtimes {
            if !runtime_ids.insert(runtime.runtime_id.as_str()) {
                return Err(ValidationError::invalid(
                    "runtimes",
                    "contains a duplicate runtimeId",
                ));
            }
            limits::validate_version_label("runtimeVersion", &runtime.runtime_version)?;
        }

        let mut project_ids: HashSet<&str> = HashSet::new();
        for project in &self.projects {
            if !project_ids.insert(project.project_id.as_str()) {
                return Err(ValidationError::invalid(
                    "projects",
                    "contains a duplicate projectId",
                ));
            }
            if let Some(label) = &project.label {
                limits::validate_short_text("label", label)?;
            }
        }

        let mut session_ids: HashSet<&str> = HashSet::new();
        for session in &self.sessions {
            if !session_ids.insert(session.session_id.as_str()) {
                return Err(ValidationError::invalid(
                    "sessions",
                    "contains a duplicate sessionId",
                ));
            }
            if !project_ids.contains(session.project_id.as_str()) {
                return Err(ValidationError::invalid(
                    "sessions",
                    "a session references an unknown project",
                ));
            }
            if !runtime_ids.contains(session.runtime_id.as_str()) {
                return Err(ValidationError::invalid(
                    "sessions",
                    "a session references an unknown runtime",
                ));
            }
            validate_capabilities("declaredCapabilities", &session.declared_capabilities)?;
        }
        Ok(())
    }
}

/// Richiesta di collegamento a una sessione.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attach {
    pub session_id: SessionId,
}

impl Attach {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            session_id: reader.take("sessionId")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Richiesta di scollegamento da una sessione.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detach {
    pub session_id: SessionId,
}

impl Detach {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            session_id: reader.take("sessionId")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Cambiamento di una sessione già scoperta.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionEventKind {
    Updated {
        state: SessionState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<SessionMode>,
    },
    Ended {
        completion: CompletionReason,
    },
}

/// Evento di sessione dell'adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEvent {
    pub session_id: SessionId,
    /// Revisione della sessione, distinta dall'identificatore del messaggio.
    pub revision: u64,
    pub event: SessionEventKind,
}

impl SessionEvent {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            session_id: reader.take("sessionId")?,
            revision: reader.take("revision")?,
            event: reader.take("event")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if self.revision == 0 {
            return Err(ValidationError::invalid(
                "revision",
                "must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Azione strutturata richiesta dall'hub. Non è un comando shell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OperationBody {
    Approve {
        request_id: RequestId,
        request_revision: u64,
    },
    Reject {
        request_id: RequestId,
        request_revision: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Reply {
        request_id: RequestId,
        text: String,
    },
    Stop,
}

impl OperationBody {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Approve {
                request_revision, ..
            }
            | Self::Reject {
                request_revision, ..
            } => {
                if *request_revision == 0 {
                    return Err(ValidationError::invalid(
                        "operation.requestRevision",
                        "must be greater than zero",
                    ));
                }
            }
            Self::Reply { text, .. } => {
                limits::validate_reply_text("operation.text", text)?;
            }
            Self::Stop => {}
        }
        if let Self::Reject {
            reason: Some(reason),
            ..
        } = self
        {
            limits::validate_short_text("operation.reason", reason)?;
        }
        Ok(())
    }
}

/// Operazione dell'hub verso l'adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub operation_id: OperationId,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_unix_ms: Option<u64>,
    pub operation: OperationBody,
}

impl Operation {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            operation_id: reader.take("operationId")?,
            session_id: reader.take("sessionId")?,
            expires_at_unix_ms: reader.take_optional("expiresAtUnixMs")?,
            operation: reader.take("operation")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if let Some(expires_at) = self.expires_at_unix_ms
            && expires_at == 0
        {
            return Err(ValidationError::invalid(
                "expiresAtUnixMs",
                "must be greater than zero",
            ));
        }
        self.operation.validate()
    }
}

/// Accettazione dell'operazione da parte dell'adapter: non è il suo esito.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationAccepted {
    pub operation_id: OperationId,
}

impl OperationAccepted {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            operation_id: reader.take("operationId")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Rifiuto immediato dell'operazione, prima di raggiungere il runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationRejected {
    pub operation_id: OperationId,
    pub error: ProtocolError,
}

impl OperationRejected {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            operation_id: reader.take("operationId")?,
            error: reader.take("error")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        self.error.validate()
    }
}

/// Motivo di un esito che l'adapter non può stabilire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnknownOutcomeReason {
    NoAcknowledgement,
    ConnectionLost,
    RuntimeUnobservable,
}

/// Esito di un'operazione. Un fallimento certo è distinto da un esito incerto.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OperationOutcome {
    Succeeded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Failed {
        error: ProtocolError,
    },
    OutcomeUnknown {
        reason: UnknownOutcomeReason,
    },
}

impl OperationOutcome {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Succeeded { summary } => {
                if let Some(summary) = summary {
                    limits::validate_short_text("outcome.summary", summary)?;
                }
            }
            Self::Failed { error } => error.validate()?,
            Self::OutcomeUnknown { .. } => {}
        }
        Ok(())
    }
}

/// Esito dell'operazione comunicato dall'adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationResult {
    pub operation_id: OperationId,
    pub outcome: OperationOutcome,
}

impl OperationResult {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            operation_id: reader.take("operationId")?,
            outcome: reader.take("outcome")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        self.outcome.validate()
    }
}

/// Stato di salute dichiarato dall'adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub state: HealthState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Health {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            state: reader.take("state")?,
            detail: reader.take_optional("detail")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if let Some(detail) = &self.detail {
            limits::validate_short_text("detail", detail)?;
        }
        Ok(())
    }
}

/// Ping bidirezionale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ping {
    pub nonce: u64,
}

impl Ping {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            nonce: reader.take("nonce")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Risposta a un ping, con lo stesso nonce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pong {
    pub nonce: u64,
}

impl Pong {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            nonce: reader.take("nonce")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Richiesta o notifica di arresto ordinato.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Shutdown {
    pub reason: ShutdownReason,
}

impl Shutdown {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            reason: reader.take("reason")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        Ok(())
    }
}

/// Errore applicativo correlabile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorMessage {
    pub error: ProtocolError,
}

impl ErrorMessage {
    pub(crate) fn from_payload(reader: &mut PayloadReader) -> Result<Self, DecodeError> {
        Ok(Self {
            error: reader.take("error")?,
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        self.error.validate()
    }
}

fn validate_capabilities(
    field: &str,
    capabilities: &[CapabilityId],
) -> Result<(), ValidationError> {
    limits::check_len(field, capabilities.len(), limits::MAX_DECLARED_CAPABILITIES)?;
    let mut seen: HashSet<&str> = HashSet::new();
    for capability in capabilities {
        if !seen.insert(capability.as_str()) {
            return Err(ValidationError::invalid(
                field,
                "contains a duplicate capability",
            ));
        }
    }
    Ok(())
}
