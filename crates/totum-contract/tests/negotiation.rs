//! Regole verificabili di versione, negoziazione e handshake adapter–hub.

use totum_contract::{
    ADAPTER_SUPPORTED_RANGE, ADMIN_SUPPORTED_RANGE, AdapterHello, AdapterInstanceId, CapabilityId,
    ErrorCode, ProductId, ProtocolRange, TechnicalSurfaceId, Version, negotiate,
    negotiate_adapter_handshake,
};

fn version(major: u16, minor: u16) -> Version {
    Version::new(major, minor)
}

fn range(min: (u16, u16), max: (u16, u16)) -> ProtocolRange {
    ProtocolRange::new(version(min.0, min.1), version(max.0, max.1))
}

fn capability(value: &str) -> CapabilityId {
    CapabilityId::new(value).expect("synthetic capability identifier")
}

fn hello(declared: &[&str]) -> AdapterHello {
    AdapterHello {
        adapter_instance_id: AdapterInstanceId::new("adapter-1").expect("synthetic adapter id"),
        product_id: ProductId::new("example-product").expect("synthetic product id"),
        technical_surface_id: TechnicalSurfaceId::new("example-surface")
            .expect("synthetic surface id"),
        adapter_version: "0.3.0".to_string(),
        supported_protocol: range((1, 0), (1, 0)),
        declared_capabilities: declared.iter().map(|value| capability(value)).collect(),
    }
}

#[test]
fn minor_versions_negotiate_to_the_highest_common() {
    let ours = range((1, 0), (1, 3));
    let theirs = range((1, 2), (1, 9));
    let selected = negotiate(ours, theirs).expect("ranges overlap");
    assert_eq!(selected, version(1, 3));
    assert!(ours.contains(selected));
    assert!(theirs.contains(selected));
}

#[test]
fn a_different_major_is_never_compatible() {
    let other_major = range((2, 0), (2, 5));
    let error = negotiate(ADMIN_SUPPORTED_RANGE, other_major).expect_err("major 2 is unknown");
    let protocol_error: totum_contract::ProtocolError = error.into();
    assert_eq!(protocol_error.code, ErrorCode::ProtocolIncompatible);
    assert!(!protocol_error.retryable);

    assert!(!version(1, 0).is_compatible_with(version(2, 0)));
    assert!(!ADMIN_SUPPORTED_RANGE.contains(version(2, 0)));
    assert!(!ADAPTER_SUPPORTED_RANGE.contains(version(0, 9)));
}

#[test]
fn disjoint_minor_ranges_have_no_common_version() {
    assert!(negotiate(range((1, 0), (1, 1)), range((1, 5), (1, 9))).is_err());
    assert!(negotiate(range((1, 2), (1, 1)), range((1, 0), (1, 9))).is_err());
}

#[test]
fn invalid_versions_and_ranges_are_refused_in_decoding() {
    assert!(serde_json::from_str::<Version>(r#"{"major":0,"minor":1}"#).is_err());
    assert!(serde_json::from_str::<Version>(r#"{"major":1,"minor":1000}"#).is_err());
    assert!(
        serde_json::from_str::<ProtocolRange>(
            r#"{"min":{"major":1,"minor":2},"max":{"major":1,"minor":1}}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<ProtocolRange>(
            r#"{"min":{"major":1,"minor":0},"max":{"major":2,"minor":0}}"#
        )
        .is_err()
    );
}

#[test]
fn handshake_recognizes_only_known_declared_capabilities() {
    let adapter = hello(&["discoverSessions", "stop", "futureCapability"]);
    let hub_supported = range((1, 0), (1, 5));
    let response = negotiate_adapter_handshake(&adapter, hub_supported).expect("compatible");

    assert_eq!(response.selected_version, version(1, 0));
    assert_eq!(response.hub_supported, hub_supported);
    let recognized: Vec<&str> = response
        .recognized_capabilities
        .iter()
        .map(CapabilityId::as_str)
        .collect();
    assert_eq!(recognized, ["discoverSessions", "stop"]);

    adapter
        .verify_hub_hello(&response)
        .expect("the hub hello matches the negotiated handshake");
}

#[test]
fn hub_hello_must_select_the_highest_common_version() {
    let adapter = hello(&["discoverSessions"]);
    let hub_supported = range((1, 0), (1, 5));
    let response = negotiate_adapter_handshake(&adapter, hub_supported).expect("compatible");

    let mut too_new = response.clone();
    too_new.selected_version = version(1, 1);
    let error = adapter
        .verify_hub_hello(&too_new)
        .expect_err("1.1 exceeds what the adapter supports");
    assert_eq!(error.code, ErrorCode::ProtocolIncompatible);

    let mut outside_range = response.clone();
    outside_range.selected_version = version(1, 0);
    outside_range.hub_supported = range((1, 2), (1, 5));
    let error = adapter
        .verify_hub_hello(&outside_range)
        .expect_err("the selected version is outside the hub range");
    assert_eq!(error.code, ErrorCode::InvalidPayload);
}

#[test]
fn an_unknown_capability_is_never_recognized_even_if_declared() {
    let adapter = hello(&["discoverSessions", "futureCapability"]);
    let response =
        negotiate_adapter_handshake(&adapter, ADAPTER_SUPPORTED_RANGE).expect("compatible");
    let recognized: Vec<&str> = response
        .recognized_capabilities
        .iter()
        .map(CapabilityId::as_str)
        .collect();
    assert_eq!(recognized, ["discoverSessions"]);

    let mut tampered = response.clone();
    tampered
        .recognized_capabilities
        .push(capability("futureCapability"));
    let error = adapter
        .verify_hub_hello(&tampered)
        .expect_err("recognition is limited to the capabilities of this revision");
    assert_eq!(error.code, ErrorCode::InvalidPayload);
}

#[test]
fn hub_cannot_recognize_a_capability_the_adapter_did_not_declare() {
    let adapter = hello(&["discoverSessions", "stop"]);
    let response =
        negotiate_adapter_handshake(&adapter, ADAPTER_SUPPORTED_RANGE).expect("compatible");

    let mut tampered = response.clone();
    tampered.recognized_capabilities.push(capability("pause"));
    let error = adapter
        .verify_hub_hello(&tampered)
        .expect_err("pause was not declared");
    assert_eq!(error.code, ErrorCode::InvalidPayload);
}

#[test]
fn an_unknown_major_handshake_is_refused() {
    let mut future = hello(&["discoverSessions"]);
    future.supported_protocol = range((2, 0), (2, 0));
    let error = negotiate_adapter_handshake(&future, ADAPTER_SUPPORTED_RANGE)
        .expect_err("major 2 is unknown");
    assert_eq!(error.code, ErrorCode::ProtocolIncompatible);
    assert!(!error.retryable);
}
