//! Codifica del valore `Run` di Windows.
//!
//! Il valore è una riga di comando senza argomenti. Il percorso è racchiuso fra
//! virgolette doppie soltanto quando contiene spazi o tabulazioni, la forma che
//! il parser dei programmi C riconosce come un unico argomento. Il tipo del
//! valore è `REG_SZ`: non viene richiesta l'espansione di variabili d'ambiente,
//! quindi `%` e `$` restano letterali. Un nome di file Windows non può
//! contenere una virgoletta doppia, che qui è rifiutata come non
//! rappresentabile.
//!
//! La validazione del percorso segue le regole Windows anche quando il modulo
//! viene compilato per le prove su un'altra piattaforma: non usa
//! `Path::is_absolute`, che dipende dal sistema ospite.

use std::path::{Path, PathBuf};

use super::{AutostartError, Mechanism};

/// Nome dedicato del valore sotto la chiave `Run` dell'utente.
pub(super) const VALUE_NAME: &str = "app.homun.totum.daemon";

/// Lunghezza massima documentata della riga di comando.
const MAX_COMMAND_LINE: usize = 260;

/// Costruisce il testo del valore per il percorso indicato.
pub(super) fn encode(program: &Path) -> Result<String, AutostartError> {
    let text = windows_text(program)?;
    if text.contains('"') {
        return Err(AutostartError::NotRepresentable {
            path: program.to_path_buf(),
            mechanism: Mechanism::RunKey,
            detail: "un nome di file Windows non può contenere una virgoletta doppia".to_owned(),
        });
    }

    let command = if text
        .chars()
        .any(|character| matches!(character, ' ' | '\t'))
    {
        format!("\"{text}\"")
    } else {
        text
    };
    if command.encode_utf16().count() > MAX_COMMAND_LINE {
        return Err(AutostartError::NotRepresentable {
            path: program.to_path_buf(),
            mechanism: Mechanism::RunKey,
            detail: format!("la riga di comando supera {MAX_COMMAND_LINE} caratteri UTF-16"),
        });
    }
    Ok(command)
}

/// Riconosce il percorso di un valore prodotto da [`encode`].
///
/// Una riga con più argomenti, una virgoletta non bilanciata, un percorso
/// relativo o una codifica non canonica non sono riconosciuti.
pub(super) fn parse_owned(command: &str) -> Option<PathBuf> {
    let text = if let Some(inner) = command.strip_prefix('"') {
        let inner = inner.strip_suffix('"')?;
        if inner.contains('"') {
            return None;
        }
        inner
    } else {
        if command.contains('"')
            || command
                .chars()
                .any(|character| matches!(character, ' ' | '\t'))
        {
            return None;
        }
        command
    };
    if text.is_empty() {
        return None;
    }
    let path = PathBuf::from(text);
    windows_text(&path).ok()?;
    if encode(&path).ok().as_deref() != Some(command) {
        return None;
    }
    Some(path)
}

fn windows_text(path: &Path) -> Result<String, AutostartError> {
    let text = path
        .to_str()
        .ok_or_else(|| AutostartError::NotRepresentable {
            path: path.to_path_buf(),
            mechanism: Mechanism::RunKey,
            detail: "il percorso non è UTF-8".to_owned(),
        })?;
    if !is_absolute_windows(text) {
        return Err(AutostartError::InvalidProgram {
            path: path.to_path_buf(),
            reason: "il percorso deve essere assoluto (unità o UNC)".to_owned(),
        });
    }
    if text.ends_with('/') || text.ends_with('\\') {
        return Err(AutostartError::InvalidProgram {
            path: path.to_path_buf(),
            reason: "il percorso termina con un separatore".to_owned(),
        });
    }
    if text
        .split(['/', '\\'])
        .any(|part| part == "." || part == "..")
    {
        return Err(AutostartError::InvalidProgram {
            path: path.to_path_buf(),
            reason: "il percorso non può contenere '.' o '..'".to_owned(),
        });
    }
    if text.chars().any(char::is_control) {
        return Err(AutostartError::NotRepresentable {
            path: path.to_path_buf(),
            mechanism: Mechanism::RunKey,
            detail: "il percorso contiene caratteri di controllo".to_owned(),
        });
    }
    Ok(text.to_owned())
}

fn is_absolute_windows(text: &str) -> bool {
    let bytes = text.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    let unc = text.starts_with("\\\\");
    drive || unc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_covers_spaces_unicode_percent_and_dollar() {
        for (text, expected) in [
            (
                r"C:\Program Files\Totum\totum-daemon.exe",
                Some(r#""C:\Program Files\Totum\totum-daemon.exe""#),
            ),
            (r"C:\Totum\daemon.exe", Some(r"C:\Totum\daemon.exe")),
            (
                r"C:\Totum dæmon\daemon.exe",
                Some(r#""C:\Totum dæmon\daemon.exe""#),
            ),
            (r"C:\100%\daemon.exe", Some(r"C:\100%\daemon.exe")),
            (r"C:\$HOME\daemon.exe", Some(r"C:\$HOME\daemon.exe")),
            ("C:\\Totum\"quote\"\\daemon.exe", None),
            ("relative.exe", None),
            (r"C:\Totum\..\daemon.exe", None),
        ] {
            let result = encode(Path::new(text));
            assert_eq!(result.ok().as_deref(), expected, "{text}");
        }
    }

    #[test]
    fn parse_owned_rejects_arguments_and_foreign_forms() {
        assert_eq!(
            parse_owned(r#""C:\Program Files\Totum\daemon.exe""#),
            Some(PathBuf::from(r"C:\Program Files\Totum\daemon.exe"))
        );
        assert_eq!(
            parse_owned(r"C:\Totum\daemon.exe"),
            Some(PathBuf::from(r"C:\Totum\daemon.exe"))
        );
        for foreign in [
            r#""C:\Program Files\Totum\daemon.exe" --flag"#,
            r#"C:\Program Files\Totum\daemon.exe"#,
            r#""C:\Totum\daemon.exe"#,
            r#""C:\Totum"\daemon.exe"#,
            r#""relative.exe""#,
            "",
        ] {
            assert_eq!(parse_owned(foreign), None, "{foreign}");
        }
    }

    #[test]
    fn command_line_length_is_enforced_in_utf16_units() {
        let prefix = r"C:\";
        let long = format!("{prefix}{}", "a".repeat(MAX_COMMAND_LINE));
        assert!(encode(Path::new(&long)).is_err());
        let fits = format!("{prefix}{}", "a".repeat(MAX_COMMAND_LINE - prefix.len()));
        assert!(encode(Path::new(&fits)).is_ok());
    }

    #[test]
    fn value_name_is_dedicated_and_not_a_command() {
        assert_eq!(VALUE_NAME, "app.homun.totum.daemon");
        assert!(!VALUE_NAME.contains('\\'));
    }
}
