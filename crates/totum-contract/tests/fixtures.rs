//! Fixture sintetiche delle superfici A e C: i test verificano i valori
//! decodificati, gli errori strutturati e l'assenza di panici sugli input
//! esterni malformati.

use std::path::{Path, PathBuf};

use totum_contract::{
    ADAPTER_SUPPORTED_RANGE, AdapterMessage, AdminMessage, ApiSurface, Body, CorrelationId,
    DecodeError, EncodeError, ErrorCode, Fingerprint, IdentityState, MAX_MESSAGE_BYTES, Message,
    MessageId, MessageType, OperationBody, OperationOutcome, ProtocolRange, RequestId,
    SessionEventKind, SessionMode, SessionState, UnknownOutcomeReason, ValidationError, Version,
    decode, encode,
};

const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

const VALID_FIXTURES: [&str; 21] = [
    "valid/admin-get-status.json",
    "valid/admin-status-report.json",
    "valid/admin-get-hub-identity.json",
    "valid/admin-hub-identity-not-initialized.json",
    "valid/adapter-hello.json",
    "valid/hub-hello.json",
    "valid/discover.json",
    "valid/discovery-result.json",
    "valid/attach.json",
    "valid/detach.json",
    "valid/session-event.json",
    "valid/operation-approve.json",
    "valid/operation-accepted.json",
    "valid/operation-rejected.json",
    "valid/operation-result-succeeded.json",
    "valid/operation-result-outcome-unknown.json",
    "valid/health.json",
    "valid/ping.json",
    "valid/pong.json",
    "valid/shutdown.json",
    "valid/error.json",
];

fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

fn fixture_bytes(relative: &str) -> Vec<u8> {
    std::fs::read(fixture_path(relative))
        .unwrap_or_else(|error| panic!("cannot read fixture {relative}: {error}"))
}

fn decode_fixture(relative: &str) -> Message {
    decode(&fixture_bytes(relative))
        .unwrap_or_else(|error| panic!("fixture {relative} must decode: {error}"))
}

fn decode_error(relative: &str) -> DecodeError {
    match decode(&fixture_bytes(relative)) {
        Ok(message) => panic!("fixture {relative} must be refused, decoded {message:?}"),
        Err(error) => error,
    }
}

fn admin(message: &Message) -> &AdminMessage {
    match &message.body {
        Body::Admin(admin) => admin,
        Body::Adapter(body) => panic!("expected an admin body, found {body:?}"),
    }
}

fn adapter(message: &Message) -> &AdapterMessage {
    match &message.body {
        Body::Adapter(adapter) => adapter,
        Body::Admin(body) => panic!("expected an adapter body, found {body:?}"),
    }
}

fn identifier(value: &str) -> MessageId {
    MessageId::new(value).expect("synthetic fixture identifier must be valid")
}

#[test]
fn admin_fixtures_decode_with_expected_values() {
    let message = decode_fixture("valid/admin-get-status.json");
    assert_eq!(message.envelope.surface, ApiSurface::Admin);
    assert_eq!(message.envelope.version, Version::new(1, 0));
    assert_eq!(
        message.envelope.message_id,
        identifier("fixture-admin-request")
    );
    assert_eq!(message.envelope.message_type, MessageType::GetStatus);
    assert_eq!(message.envelope.correlation_id, None);
    assert!(matches!(admin(&message), AdminMessage::GetStatus(_)));

    let message = decode_fixture("valid/admin-status-report.json");
    assert_eq!(
        message
            .envelope
            .correlation_id
            .as_ref()
            .map(CorrelationId::as_str),
        Some("fixture-admin-request")
    );
    match admin(&message) {
        AdminMessage::StatusReport(report) => {
            assert_eq!(report.hub_version, "0.1.0");
            assert_eq!(
                report.identity,
                IdentityState::Present {
                    fingerprint: Fingerprint::new(FINGERPRINT).expect("fixture fingerprint")
                }
            );
        }
        other => panic!("expected a status report, found {other:?}"),
    }

    let message = decode_fixture("valid/admin-get-hub-identity.json");
    assert!(matches!(admin(&message), AdminMessage::GetHubIdentity(_)));

    let message = decode_fixture("valid/admin-hub-identity-not-initialized.json");
    match admin(&message) {
        AdminMessage::HubIdentityReport(report) => {
            assert_eq!(report.identity, IdentityState::NotInitialized);
        }
        other => panic!("expected a hub identity report, found {other:?}"),
    }
}

#[test]
fn adapter_handshake_fixtures_decode_with_expected_values() {
    let message = decode_fixture("valid/adapter-hello.json");
    assert_eq!(message.envelope.surface, ApiSurface::Adapter);
    match adapter(&message) {
        AdapterMessage::AdapterHello(hello) => {
            assert_eq!(hello.adapter_instance_id.as_str(), "adapter-1");
            assert_eq!(hello.product_id.as_str(), "example-product");
            assert_eq!(hello.technical_surface_id.as_str(), "example-surface");
            assert_eq!(hello.adapter_version, "0.3.0");
            assert_eq!(
                hello.supported_protocol,
                ProtocolRange::new(Version::new(1, 0), Version::new(1, 0))
            );
            assert_eq!(hello.declared_capabilities.len(), 8);
            let future = &hello.declared_capabilities[7];
            assert_eq!(future.as_str(), "futureCapability");
            assert!(!future.is_known());
        }
        other => panic!("expected an adapter hello, found {other:?}"),
    }

    let message = decode_fixture("valid/hub-hello.json");
    match adapter(&message) {
        AdapterMessage::HubHello(hello) => {
            assert_eq!(hello.selected_version, Version::new(1, 0));
            assert_eq!(
                hello.hub_supported,
                ProtocolRange::new(Version::new(1, 0), Version::new(1, 0))
            );
            assert_eq!(hello.recognized_capabilities.len(), 7);
        }
        other => panic!("expected a hub hello, found {other:?}"),
    }
}

#[test]
fn discovery_and_attachment_fixtures_decode_with_expected_values() {
    let message = decode_fixture("valid/discover.json");
    match adapter(&message) {
        AdapterMessage::Discover(discover) => {
            assert_eq!(discover.discovery_id.as_str(), "discovery-1");
        }
        other => panic!("expected a discover, found {other:?}"),
    }

    let message = decode_fixture("valid/discovery-result.json");
    match adapter(&message) {
        AdapterMessage::DiscoveryResult(result) => {
            assert_eq!(result.discovery_id.as_str(), "discovery-1");
            assert_eq!(result.runtimes.len(), 1);
            assert_eq!(result.runtimes[0].runtime_id.as_str(), "runtime-1");
            assert_eq!(result.runtimes[0].runtime_version, "1.4.2");
            assert_eq!(result.projects.len(), 1);
            assert_eq!(result.projects[0].label.as_deref(), Some("Alpha"));
            assert_eq!(result.sessions.len(), 1);
            let session = &result.sessions[0];
            assert_eq!(session.session_id.as_str(), "session-1");
            assert_eq!(session.mode, SessionMode::Managed);
            assert_eq!(session.state, SessionState::Approval);
            assert_eq!(
                session.conversation.conversation_id.as_str(),
                "conversation-1"
            );
            assert_eq!(session.execution.execution_id.as_str(), "execution-1");
            assert_eq!(session.declared_capabilities.len(), 3);
        }
        other => panic!("expected a discovery result, found {other:?}"),
    }

    for (fixture, expected) in [
        ("valid/attach.json", "session-1"),
        ("valid/detach.json", "session-1"),
    ] {
        let message = decode_fixture(fixture);
        match adapter(&message) {
            AdapterMessage::Attach(attach) => assert_eq!(attach.session_id.as_str(), expected),
            AdapterMessage::Detach(detach) => assert_eq!(detach.session_id.as_str(), expected),
            other => panic!("expected attach or detach, found {other:?}"),
        }
    }
}

#[test]
fn event_and_operation_fixtures_decode_with_expected_values() {
    let message = decode_fixture("valid/session-event.json");
    match adapter(&message) {
        AdapterMessage::SessionEvent(event) => {
            assert_eq!(event.session_id.as_str(), "session-1");
            assert_eq!(event.revision, 3);
            assert_eq!(
                event.event,
                SessionEventKind::Updated {
                    state: SessionState::Working,
                    mode: Some(SessionMode::Managed)
                }
            );
        }
        other => panic!("expected a session event, found {other:?}"),
    }

    let message = decode_fixture("valid/operation-approve.json");
    match adapter(&message) {
        AdapterMessage::Operation(operation) => {
            assert_eq!(operation.operation_id.as_str(), "operation-1");
            assert_eq!(operation.session_id.as_str(), "session-1");
            assert_eq!(operation.expires_at_unix_ms, Some(1_893_456_000_000));
            assert_eq!(
                operation.operation,
                OperationBody::Approve {
                    request_id: RequestId::new("request-1").expect("fixture request id"),
                    request_revision: 2
                }
            );
        }
        other => panic!("expected an operation, found {other:?}"),
    }

    let message = decode_fixture("valid/operation-accepted.json");
    match adapter(&message) {
        AdapterMessage::OperationAccepted(accepted) => {
            assert_eq!(accepted.operation_id.as_str(), "operation-1");
        }
        other => panic!("expected an accepted operation, found {other:?}"),
    }

    let message = decode_fixture("valid/operation-rejected.json");
    match adapter(&message) {
        AdapterMessage::OperationRejected(rejected) => {
            assert_eq!(rejected.operation_id.as_str(), "operation-2");
            assert_eq!(rejected.error.code, ErrorCode::TargetUnavailable);
            assert!(rejected.error.retryable);
            assert_eq!(
                rejected.error.detail.as_deref(),
                Some("runtime not reachable")
            );
        }
        other => panic!("expected a rejected operation, found {other:?}"),
    }

    let message = decode_fixture("valid/operation-result-succeeded.json");
    match adapter(&message) {
        AdapterMessage::OperationResult(result) => {
            assert_eq!(result.operation_id.as_str(), "operation-1");
            assert_eq!(
                result.outcome,
                OperationOutcome::Succeeded {
                    summary: Some("approval applied".to_string())
                }
            );
        }
        other => panic!("expected an operation result, found {other:?}"),
    }

    let message = decode_fixture("valid/operation-result-outcome-unknown.json");
    match adapter(&message) {
        AdapterMessage::OperationResult(result) => {
            assert_eq!(result.operation_id.as_str(), "operation-3");
            assert_eq!(
                result.outcome,
                OperationOutcome::OutcomeUnknown {
                    reason: UnknownOutcomeReason::NoAcknowledgement
                }
            );
        }
        other => panic!("expected an operation result, found {other:?}"),
    }
}

#[test]
fn liveness_and_error_fixtures_decode_with_expected_values() {
    let message = decode_fixture("valid/health.json");
    match adapter(&message) {
        AdapterMessage::Health(health) => {
            assert_eq!(health.state, totum_contract::HealthState::Degraded);
            assert_eq!(
                health.detail.as_deref(),
                Some("runtime reported a slow approval relay")
            );
        }
        other => panic!("expected health, found {other:?}"),
    }

    for fixture in ["valid/ping.json", "valid/pong.json"] {
        let message = decode_fixture(fixture);
        match adapter(&message) {
            AdapterMessage::Ping(ping) => assert_eq!(ping.nonce, 42),
            AdapterMessage::Pong(pong) => assert_eq!(pong.nonce, 42),
            other => panic!("expected ping or pong, found {other:?}"),
        }
    }

    let message = decode_fixture("valid/shutdown.json");
    match adapter(&message) {
        AdapterMessage::Shutdown(shutdown) => {
            assert_eq!(shutdown.reason, totum_contract::ShutdownReason::Restarting);
        }
        other => panic!("expected a shutdown, found {other:?}"),
    }

    let message = decode_fixture("valid/error.json");
    match adapter(&message) {
        AdapterMessage::Error(error) => {
            assert_eq!(error.error.code, ErrorCode::InvalidPayload);
            assert!(!error.error.retryable);
            assert_eq!(
                error.error.detail.as_deref(),
                Some("the operation does not match any request of the session")
            );
        }
        other => panic!("expected an error, found {other:?}"),
    }
}

#[test]
fn invalid_fixtures_are_refused_with_structured_errors() {
    assert!(matches!(
        decode_error("invalid/unknown-surface.json"),
        DecodeError::UnknownSurface { value } if value == "relay"
    ));
    assert!(matches!(
        decode_error("invalid/unknown-type.json"),
        DecodeError::UnknownMessageType { surface: ApiSurface::Adapter, message_type }
            if message_type == "getStatus"
    ));
    assert!(matches!(
        decode_error("invalid/missing-id.json"),
        DecodeError::MissingField { field: "id" }
    ));
    assert!(matches!(
        decode_error("invalid/missing-required-field.json"),
        DecodeError::MissingField {
            field: "hubVersion"
        }
    ));
    assert!(matches!(
        decode_error("invalid/wrong-type-field.json"),
        DecodeError::InvalidField { field, .. } if field == "revision"
    ));
    assert!(matches!(
        decode_error("invalid/malformed.json"),
        DecodeError::InvalidJson { .. }
    ));
    assert!(matches!(
        decode_error("invalid/control-character.json"),
        DecodeError::InvalidField { field, reason }
            if field == "operation.text" && reason.contains("control character")
    ));
    assert!(matches!(
        decode_error("invalid/invalid-fingerprint.json"),
        DecodeError::InvalidField { field, .. } if field == "identity"
    ));
    assert!(matches!(
        decode_error("invalid/missing-correlation.json"),
        DecodeError::MissingField {
            field: "correlationId"
        }
    ));
    assert!(matches!(
        decode_error("invalid/polluted-correlation.json"),
        DecodeError::InvalidField { field, .. } if field == "correlationId"
    ));
    assert!(matches!(
        decode_error("invalid/invalid-version-range.json"),
        DecodeError::InvalidField { field, .. } if field == "supportedProtocol"
    ));
    assert!(matches!(
        decode_error("invalid/duplicate-capability.json"),
        DecodeError::InvalidField { field, .. } if field == "declaredCapabilities"
    ));
    assert!(matches!(
        decode_error("invalid/unknown-recognized-capability.json"),
        DecodeError::InvalidField { field, .. } if field == "recognizedCapabilities"
    ));
}

#[test]
fn envelope_and_body_must_agree_before_encoding() {
    let ping = decode_fixture("valid/ping.json");

    let mut renamed = ping.clone();
    renamed.envelope.message_type = MessageType::Pong;
    assert_eq!(renamed.body.message_type(), MessageType::Ping);
    assert!(matches!(
        renamed.validate(),
        Err(ValidationError::InvalidField { field, .. }) if field == "type"
    ));
    match encode(&renamed) {
        Err(EncodeError::InvalidMessage(ValidationError::InvalidField { field, .. })) => {
            assert_eq!(field, "type");
        }
        other => panic!("an envelope that renames its body must be refused, found {other:?}"),
    }

    let mut moved = ping.clone();
    moved.envelope.surface = ApiSurface::Admin;
    match encode(&moved) {
        Err(EncodeError::InvalidMessage(ValidationError::InvalidField { field, .. })) => {
            assert_eq!(field, "surface");
        }
        other => panic!("a body under another surface must be refused, found {other:?}"),
    }

    let status = decode_fixture("valid/admin-get-status.json");
    let mut crossed = status.clone();
    crossed.envelope.message_type = MessageType::Ping;
    match encode(&crossed) {
        Err(EncodeError::InvalidMessage(ValidationError::InvalidField { field, .. })) => {
            assert_eq!(field, "type");
        }
        other => panic!("a type from another surface must be refused, found {other:?}"),
    }

    encode(&ping).expect("a consistent message is still encodable");
    encode(&status).expect("a consistent message is still encodable");
}

#[test]
fn incompatible_major_version_is_refused_not_negotiated() {
    match decode_error("incompatible/adapter-hello-v2.json") {
        DecodeError::IncompatibleVersion { found, supported } => {
            assert_eq!(found, Version::new(2, 0));
            assert_eq!(supported, ADAPTER_SUPPORTED_RANGE);
        }
        other => panic!("expected an incompatible version, found {other:?}"),
    }
}

#[test]
fn out_of_limit_inputs_are_refused() {
    let capabilities = (0..65)
        .map(|index| format!("\"capability-{index}\""))
        .collect::<Vec<_>>()
        .join(",");
    let too_many = format!(
        r#"{{"surface":"adapter","version":{{"major":1,"minor":0}},"id":"fixture-too-many","type":"adapterHello","adapterInstanceId":"adapter-1","productId":"example-product","technicalSurfaceId":"example-surface","adapterVersion":"0.3.0","supportedProtocol":{{"min":{{"major":1,"minor":0}},"max":{{"major":1,"minor":0}}}},"declaredCapabilities":[{capabilities}]}}"#
    );
    assert!(matches!(
        decode(too_many.as_bytes()),
        Err(DecodeError::LimitExceeded { field, limit: 64, found: 65 }) if field == "declaredCapabilities"
    ));

    let reply = "x".repeat(4097);
    let too_long = format!(
        r#"{{"surface":"adapter","version":{{"major":1,"minor":0}},"id":"fixture-too-long","type":"operation","operationId":"operation-4","sessionId":"session-1","operation":{{"kind":"reply","requestId":"request-1","text":"{reply}"}}}}"#
    );
    assert!(matches!(
        decode(too_long.as_bytes()),
        Err(DecodeError::LimitExceeded { field, limit: 4096, found: 4097 })
            if field == "operation.text"
    ));

    let oversized = format!(
        r#"{{"surface":"admin","version":{{"major":1,"minor":0}},"id":"fixture-oversized","type":"getStatus","padding":"{}"}}"#,
        "x".repeat(MAX_MESSAGE_BYTES)
    );
    assert!(matches!(
        decode(oversized.as_bytes()),
        Err(DecodeError::TooLarge {
            limit: MAX_MESSAGE_BYTES
        })
    ));
}

#[test]
fn unknown_fields_are_ignored_but_unknown_types_are_not() {
    let json = r#"{"surface":"admin","version":{"major":1,"minor":0},"id":"fixture-future","type":"getStatus","futureField":true}"#;
    let message = decode(json.as_bytes()).expect("unknown fields are ignored");
    assert_eq!(message.message_type(), MessageType::GetStatus);

    let json = r#"{"surface":"admin","version":{"major":1,"minor":0},"id":"fixture-future","correlationId":"fixture-admin-request","type":"statusReport","hubVersion":"0.1.0","identity":{"state":"notInitialized","futureNote":"ignored"}}"#;
    let message = decode(json.as_bytes()).expect("unknown payload fields are ignored");
    match admin(&message) {
        AdminMessage::StatusReport(report) => {
            assert_eq!(report.identity, IdentityState::NotInitialized);
        }
        other => panic!("expected a status report, found {other:?}"),
    }
}

#[test]
fn valid_fixtures_survive_encode_and_decode() {
    for relative in VALID_FIXTURES {
        let message = decode_fixture(relative);
        let encoded =
            encode(&message).unwrap_or_else(|error| panic!("cannot encode {relative}: {error}"));
        let decoded = decode(encoded.as_bytes())
            .unwrap_or_else(|error| panic!("cannot decode {relative} again: {error}"));
        assert_eq!(decoded, message, "round trip changed {relative}");
    }
}

#[test]
fn encoding_refuses_broken_correlation_rules() {
    let mut response = decode_fixture("valid/admin-status-report.json");
    response.envelope.correlation_id = None;
    assert!(matches!(
        encode(&response),
        Err(EncodeError::InvalidMessage(ValidationError::InvalidField { field, .. }))
            if field == "correlationId"
    ));

    let mut request = decode_fixture("valid/admin-get-status.json");
    request.envelope.correlation_id =
        Some(CorrelationId::new("fixture-other").expect("synthetic id"));
    assert!(matches!(
        encode(&request),
        Err(EncodeError::InvalidMessage(ValidationError::InvalidField { field, .. }))
            if field == "correlationId"
    ));
}

#[test]
fn malformed_bytes_are_rejected_without_panicking() {
    for relative in VALID_FIXTURES {
        let bytes = fixture_bytes(relative);
        for length in 0..bytes.len() {
            let _ = decode(&bytes[..length]);
        }
        let mut mutated = bytes;
        if let Some(last) = mutated.last_mut() {
            *last = 0xFF;
        }
        let _ = decode(&mutated);
    }

    for byte in 0..=u8::MAX {
        assert!(decode(&[byte]).is_err());
    }
}
