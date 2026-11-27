//! Limiti di lavoro dei contratti A/C.
//!
//! Sono parametri di lavoro, non valori di prodotto già misurati: servono a
//! rendere finiti gli input esterni e potranno cambiare dopo i primi
//! esperimenti. Ogni limite è espresso in byte per gli identificatori e in
//! caratteri per i testi.

use crate::error::ValidationError;

/// Dimensione massima di un messaggio codificato, envelope compreso.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Lunghezza massima in byte di un identificatore o di una capability.
pub const MAX_ID_BYTES: usize = 128;

/// Lunghezza massima in byte di un'etichetta di versione di prodotto o runtime.
pub const MAX_VERSION_LABEL_BYTES: usize = 64;

/// Lunghezza massima in byte di un'impronta dell'hub.
pub const MAX_FINGERPRINT_BYTES: usize = 128;

/// Numero massimo di caratteri di un dettaglio di errore redatto.
pub const MAX_DETAIL_CHARS: usize = 512;

/// Numero massimo di caratteri di un'etichetta breve (progetto, riassunto).
pub const MAX_SHORT_TEXT_CHARS: usize = 256;

/// Numero massimo di caratteri di un testo di risposta a una richiesta di input.
pub const MAX_REPLY_TEXT_CHARS: usize = 4096;

/// Numero massimo di capability dichiarate in un handshake o in una sessione.
pub const MAX_DECLARED_CAPABILITIES: usize = 64;

/// Numero massimo di istanze runtime in un risultato di discovery.
pub const MAX_DISCOVERED_RUNTIMES: usize = 64;

/// Numero massimo di progetti in un risultato di discovery.
pub const MAX_DISCOVERED_PROJECTS: usize = 256;

/// Numero massimo di sessioni in un risultato di discovery.
pub const MAX_DISCOVERED_SESSIONS: usize = 512;

/// Valore massimo di una componente di versione.
pub const MAX_VERSION_COMPONENT: u16 = 999;

pub(crate) fn check_len(field: &str, found: usize, limit: usize) -> Result<(), ValidationError> {
    if found > limit {
        return Err(ValidationError::LimitExceeded {
            field: field.to_string(),
            limit,
            found,
        });
    }
    Ok(())
}

pub(crate) fn validate_chars(
    field: &str,
    value: &str,
    max_chars: usize,
    allow_linebreaks: bool,
) -> Result<(), ValidationError> {
    let found = value.chars().count();
    check_len(field, found, max_chars)?;
    for character in value.chars() {
        let permitted_break = allow_linebreaks && matches!(character, '\n' | '\r' | '\t');
        if character.is_control() && !permitted_break {
            return Err(ValidationError::invalid(
                field,
                "contains a control character",
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_short_text(field: &str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::invalid(field, "must not be empty"));
    }
    validate_chars(field, value, MAX_SHORT_TEXT_CHARS, false)
}

pub(crate) fn validate_reply_text(field: &str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::invalid(field, "must not be empty"));
    }
    validate_chars(field, value, MAX_REPLY_TEXT_CHARS, true)
}

pub(crate) fn validate_detail(field: &str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::invalid(field, "must not be empty"));
    }
    validate_chars(field, value, MAX_DETAIL_CHARS, true)
}

pub(crate) fn validate_version_label(field: &str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::invalid(field, "must not be empty"));
    }
    check_len(field, value.len(), MAX_VERSION_LABEL_BYTES)?;
    let valid = value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-' | b'_'));
    if !valid {
        return Err(ValidationError::invalid(
            field,
            "contains characters outside [A-Za-z0-9.+-_]",
        ));
    }
    Ok(())
}
