//! Prove di parsing e validazione dei documenti, senza file system.

use super::document::{self, Parsed};

fn parse(text: &str) -> Parsed {
    document::parse(text.as_bytes())
}

fn assert_invalid(text: &str) {
    assert!(
        matches!(parse(text), Parsed::Invalid { .. }),
        "atteso documento non valido: {text}"
    );
}

fn assert_invalid_revision(text: &str) {
    assert!(
        matches!(parse(text), Parsed::InvalidRevision { .. }),
        "attesa revisione non valida: {text}"
    );
}

#[test]
fn current_schema_is_accepted_with_its_revision() {
    match parse(r#"{"schemaVersion":1,"revision":7}"#) {
        Parsed::Current(config) => {
            assert_eq!(config.schema_version(), 1);
            assert_eq!(config.revision(), 7);
        }
        _ => panic!("documento v1 valido rifiutato"),
    }
}

#[test]
fn trailing_whitespace_is_not_part_of_the_document() {
    let text = format!("{{\"schemaVersion\":1,\"revision\":0}}{}", " ".repeat(1024));
    assert!(matches!(parse(&text), Parsed::Current(_)));
}

#[test]
fn field_names_are_exact_and_unknown_fields_are_refused() {
    assert_invalid(r#"{"schema_version":1,"revision":0}"#);
    assert_invalid(r#"{"schemaVersion":1,"revision":0,"schema_version":1}"#);
    assert_invalid(r#"{"schemaVersion":1,"revision":0,"enabled":true}"#);
    assert_invalid(r#"{"schemaVersion":1,"revision":0,"Revision":0}"#);
}

#[test]
fn documents_that_are_not_objects_are_never_valid_or_migratable() {
    // La deserializzazione derivata accetterebbe una sequenza posizionale: la
    // forma oggetto va imposta prima di interpretare lo schema.
    for text in [
        "[0]", "[0,0]", "[1]", "[1,0]", "[2]", "[2,0]", "[]", "0", "1", "2", "null", "true",
        "\"0\"",
    ] {
        assert_invalid(text);
    }
}

#[test]
fn synthetic_v0_tolerates_formatting_whitespace() {
    for text in [
        "{\n  \"schemaVersion\" : 0\n}",
        " \t\r\n{\"schemaVersion\":0}\n",
        "{\"schemaVersion\":0}\r\n",
    ] {
        assert!(
            matches!(parse(text), Parsed::Migratable(_)),
            "documento v0 formattato rifiutato: {text:?}"
        );
    }
}

#[test]
fn duplicate_fields_are_refused_even_when_the_first_is_valid() {
    assert_invalid(r#"{"schemaVersion":1,"schemaVersion":1,"revision":0}"#);
    assert_invalid(r#"{"schemaVersion":1,"revision":1,"revision":2}"#);
}

#[test]
fn missing_fields_are_refused() {
    assert_invalid(r#"{"revision":0}"#);
    assert_invalid(r#"{"schemaVersion":1}"#);
}

#[test]
fn revision_must_be_a_non_negative_integer() {
    for text in [
        r#"{"schemaVersion":1,"revision":-1}"#,
        r#"{"schemaVersion":1,"revision":1.5}"#,
        r#"{"schemaVersion":1,"revision":"1"}"#,
        r#"{"schemaVersion":1,"revision":null}"#,
        r#"{"schemaVersion":1,"revision":true}"#,
        r#"{"schemaVersion":1,"revision":[1]}"#,
        r#"{"schemaVersion":1,"revision":{"value":1}}"#,
        r#"{"schemaVersion":1,"revision":18446744073709551616}"#,
    ] {
        assert_invalid_revision(text);
    }
}

#[test]
fn malformed_documents_are_refused() {
    assert_invalid("");
    assert_invalid(r#"{"schemaVersion":1,"revision":"#);
    assert_invalid(r#"{"schemaVersion":1,"revision":0}"#.trim_end_matches('}'));
    assert_invalid(r#"{"schemaVersion":1,"revision":0} {"schemaVersion":1}"#);
    assert_invalid(r#"[{"schemaVersion":1,"revision":0}]"#);
    assert_invalid(r#"{"schemaVersion":1,"revision":0"#);
    assert_invalid("null");
}

#[test]
fn schema_version_must_be_an_integer() {
    assert_invalid(r#"{"schemaVersion":"1","revision":0}"#);
    assert_invalid(r#"{"schemaVersion":-1,"revision":0}"#);
    assert_invalid(r#"{"schemaVersion":1.0,"revision":0}"#);
}

#[test]
fn future_schema_is_recognised_without_interpreting_its_fields() {
    match parse(r#"{"schemaVersion":2,"revision":5,"adapter":{"id":"x"}}"#) {
        Parsed::TooNew { schema_version } => assert_eq!(schema_version, 2),
        _ => panic!("schema futuro non riconosciuto"),
    }
}

#[test]
fn synthetic_v0_is_recognised_only_with_its_exact_document() {
    match parse(r#"{"schemaVersion":0}"#) {
        Parsed::Migratable(previous) => {
            assert_eq!(previous.schema_version(), 0);
            assert_eq!(previous.revision(), 0);
        }
        _ => panic!("schema sintetico 0 non riconosciuto"),
    }
    let spaced = format!("{{\"schemaVersion\":0}}{}", " ".repeat(64));
    assert!(matches!(parse(&spaced), Parsed::Migratable(_)));

    // Un contenuto diverso dal documento sintetico non è migrabile: è
    // corrotto, mai reinterpretato.
    for text in [
        r#"{"schemaVersion":0,"revision":0}"#,
        r#"{"schemaVersion":0,"enabled":true}"#,
        r#"{"schemaVersion":0,"schemaVersion":0}"#,
        r#"{"schemaVersion":0,"#,
        r#"{"schemaVersion":"0"}"#,
        r#"{"schemaVersion":0.0}"#,
        r#"{"schemaVersion":0} {"schemaVersion":0}"#,
    ] {
        assert_invalid(text);
    }
}

#[test]
fn the_v0_to_v1_transform_increments_the_revision() {
    let next = document::migrate_v0_to_v1(super::ConfigV0::initial()).expect("trasformazione");
    assert_eq!(next.schema_version(), 1);
    assert_eq!(next.revision(), 1);
    assert_eq!(
        document::serialize(next.revision()).expect("serializzato"),
        b"{\"schemaVersion\":1,\"revision\":1}\n".to_vec()
    );
}

#[test]
fn the_transform_refuses_to_overflow_the_revision() {
    match document::migrate_v0_to_v1(super::ConfigV0::with_revision(u64::MAX)) {
        Err(super::ConfigError::MigrationRevisionOverflow { revision }) => {
            assert_eq!(revision, u64::MAX)
        }
        Err(other) => panic!("errore inatteso: {other}"),
        Ok(_) => panic!("overflow non rilevato"),
    }
}
