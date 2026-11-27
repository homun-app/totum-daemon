//! Prove di parsing e dei valori iniziali, senza file system.

use super::document::{self, ParsedControllers, ParsedIdentity};

fn identity(text: &str) -> ParsedIdentity {
    document::parse_identity(text.as_bytes())
}

fn controllers(text: &str) -> ParsedControllers {
    document::parse_controllers(text.as_bytes())
}

#[test]
fn serialization_matches_the_documented_initial_documents() {
    assert_eq!(
        document::serialize_identity().expect("serializzato"),
        b"{\"schemaVersion\":1,\"state\":\"notInitialized\"}\n".to_vec()
    );
    assert_eq!(
        document::serialize_controllers().expect("serializzato"),
        b"{\"schemaVersion\":1,\"grantRevision\":0,\"controllers\":[]}\n".to_vec()
    );
}

#[test]
fn the_serialized_documents_are_read_back_as_initial() {
    assert!(matches!(
        identity(
            std::str::from_utf8(&document::serialize_identity().expect("serializzato"))
                .expect("utf8")
        ),
        ParsedIdentity::Initial
    ));
    assert!(matches!(
        controllers(
            std::str::from_utf8(&document::serialize_controllers().expect("serializzato"))
                .expect("utf8")
        ),
        ParsedControllers::Initial
    ));
}

#[test]
fn identity_accepts_only_the_initial_state() {
    assert!(matches!(
        identity(r#"{"schemaVersion":1,"state":"notInitialized"}"#),
        ParsedIdentity::Initial
    ));
    assert!(matches!(
        identity(r#"{"schemaVersion":1,"state":"initialized"}"#),
        ParsedIdentity::NonInitial { .. }
    ));
}

#[test]
fn controllers_accept_only_the_empty_registry_and_zero_revision() {
    assert!(matches!(
        controllers(r#"{"schemaVersion":1,"grantRevision":0,"controllers":[]}"#),
        ParsedControllers::Initial
    ));
    assert!(matches!(
        controllers(r#"{"schemaVersion":1,"grantRevision":1,"controllers":[]}"#),
        ParsedControllers::NonInitial { .. }
    ));
    assert!(matches!(
        controllers(r#"{"schemaVersion":1,"grantRevision":0,"controllers":[1]}"#),
        ParsedControllers::NonInitial { .. }
    ));
    // Lo schema delle voci non è definito: anche un oggetto è solo non iniziale.
    assert!(matches!(
        controllers(r#"{"schemaVersion":1,"grantRevision":0,"controllers":[{"id":"x"}]}"#),
        ParsedControllers::NonInitial { .. }
    ));
}

#[test]
fn documents_that_are_not_objects_are_refused() {
    for text in [
        "[1]",
        "[1,0]",
        "[]",
        "0",
        "1",
        "null",
        "true",
        "\"notInitialized\"",
    ] {
        assert!(
            matches!(identity(text), ParsedIdentity::Invalid { .. }),
            "identity: {text}"
        );
        assert!(
            matches!(controllers(text), ParsedControllers::Invalid { .. }),
            "controllers: {text}"
        );
    }
}

#[test]
fn duplicate_and_unknown_fields_are_refused() {
    for text in [
        r#"{"schemaVersion":1,"state":"notInitialized","state":"notInitialized"}"#,
        r#"{"schemaVersion":1,"schemaVersion":1,"state":"notInitialized"}"#,
        r#"{"schemaVersion":1,"state":"notInitialized","extra":true}"#,
        r#"{"schema_version":1,"state":"notInitialized"}"#,
    ] {
        assert!(
            matches!(identity(text), ParsedIdentity::Invalid { .. }),
            "identity: {text}"
        );
    }
    for text in [
        r#"{"schemaVersion":1,"grantRevision":0,"grantRevision":0,"controllers":[]}"#,
        r#"{"schemaVersion":1,"grantRevision":0,"controllers":[],"extra":1}"#,
        r#"{"schemaVersion":1,"grant_revision":0,"controllers":[]}"#,
    ] {
        assert!(
            matches!(controllers(text), ParsedControllers::Invalid { .. }),
            "controllers: {text}"
        );
    }
}

#[test]
fn missing_fields_and_wrong_types_are_refused() {
    for text in [
        r#"{"state":"notInitialized"}"#,
        r#"{"schemaVersion":1}"#,
        r#"{"schemaVersion":1,"state":1}"#,
        r#"{"schemaVersion":1,"state":null}"#,
        r#"{"schemaVersion":"1","state":"notInitialized"}"#,
        r#"{"schemaVersion":1.0,"state":"notInitialized"}"#,
        r#"{"schemaVersion":-1,"state":"notInitialized"}"#,
    ] {
        assert!(
            matches!(identity(text), ParsedIdentity::Invalid { .. }),
            "identity: {text}"
        );
    }
    for text in [
        r#"{"grantRevision":0,"controllers":[]}"#,
        r#"{"schemaVersion":1,"controllers":[]}"#,
        r#"{"schemaVersion":1,"grantRevision":0}"#,
        r#"{"schemaVersion":1,"grantRevision":-1,"controllers":[]}"#,
        r#"{"schemaVersion":1,"grantRevision":1.5,"controllers":[]}"#,
        r#"{"schemaVersion":1,"grantRevision":"0","controllers":[]}"#,
        r#"{"schemaVersion":1,"grantRevision":0,"controllers":{}}"#,
        r#"{"schemaVersion":1,"grantRevision":0,"controllers":0}"#,
    ] {
        assert!(
            matches!(controllers(text), ParsedControllers::Invalid { .. }),
            "controllers: {text}"
        );
    }
}

#[test]
fn truncated_documents_are_refused() {
    for text in [
        "",
        r#"{"schemaVersion":1,"state":"notInitialized""#,
        r#"{"schemaVersion":1,"grantRevision":0,"controllers":["#,
        r#"{"schemaVersion":1}"#,
        r#"{"schemaVersion":1} {"schemaVersion":1}"#,
    ] {
        assert!(
            matches!(identity(text), ParsedIdentity::Invalid { .. }),
            "identity: {text}"
        );
        assert!(
            matches!(controllers(text), ParsedControllers::Invalid { .. }),
            "controllers: {text}"
        );
    }
}

#[test]
fn future_and_unsupported_schemas_are_recognised_in_objects() {
    assert!(matches!(
        identity(r#"{"schemaVersion":2,"state":"notInitialized"}"#),
        ParsedIdentity::UnsupportedSchema { schema_version: 2 }
    ));
    assert!(matches!(
        identity(r#"{"schemaVersion":0,"state":"notInitialized"}"#),
        ParsedIdentity::UnsupportedSchema { schema_version: 0 }
    ));
    assert!(matches!(
        controllers(r#"{"schemaVersion":7,"grantRevision":0,"controllers":[]}"#),
        ParsedControllers::UnsupportedSchema { schema_version: 7 }
    ));
}

#[test]
fn trailing_whitespace_is_tolerated_but_not_another_document() {
    assert!(matches!(
        identity("{\"schemaVersion\":1,\"state\":\"notInitialized\"}\r\n "),
        ParsedIdentity::Initial
    ));
    assert!(matches!(
        controllers(" \t\r\n{\"schemaVersion\":1,\"grantRevision\":0,\"controllers\":[]}\n"),
        ParsedControllers::Initial
    ));
}

#[test]
fn write_temporaries_are_recognised_only_by_the_exact_pattern() {
    use std::ffi::OsStr;

    use super::write::is_reserved_temporary;

    for name in [
        ".identity.1.0.tmp",
        ".identity.4242.17.tmp",
        ".identity.0.0.tmp",
    ] {
        assert!(is_reserved_temporary(OsStr::new(name)), "{name}");
    }
    for name in [
        "identity.json",
        "controllers.json",
        "state.lock",
        ".config.1.0.tmp",
        ".identity.1.tmp",
        ".identity.tmp",
        ".identity..1.tmp",
        ".identity.1..tmp",
        ".identity.1.0.tmp.bak",
        ".identity.a.0.tmp",
    ] {
        assert!(!is_reserved_temporary(OsStr::new(name)), "{name}");
    }
}
