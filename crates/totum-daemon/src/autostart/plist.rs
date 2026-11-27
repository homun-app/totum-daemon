//! Codifica e riconoscimento del Launch Agent macOS.
//!
//! Il documento è deterministico: l'unica variabile è il percorso assoluto del
//! binario, codificato come testo XML. Le entità ammesse sono quelle prodotte
//! dal modulo; un documento che differisce per qualunque altro byte non è
//! riconosciuto come proprio e non viene mai sovrascritto o rimosso.

use std::path::{Path, PathBuf};

use super::{AutostartError, Mechanism, program_text};

/// Nome del file nella directory `LaunchAgents`.
pub(super) const FILE_NAME: &str = "app.homun.totum.daemon.plist";

const PREFIX: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>Label</key>\n\
\t<string>app.homun.totum.daemon</string>\n\
\t<key>ProgramArguments</key>\n\
\t<array>\n\
\t\t<string>";

const SUFFIX: &str = "</string>\n\
\t</array>\n\
\t<key>RunAtLoad</key>\n\
\t<true/>\n\
</dict>\n\
</plist>\n";

/// Costruisce il documento per il percorso indicato.
pub(super) fn document(program: &Path) -> Result<String, AutostartError> {
    let text = program_text(program, Mechanism::LaunchAgent)?;
    Ok(format!("{PREFIX}{}{SUFFIX}", escape(&text)))
}

/// Riconosce il percorso di un documento prodotto da [`document`].
///
/// Restituisce `None` per qualunque differenza di struttura, etichetta,
/// chiavi, ordine, spaziatura o codifica delle entità.
pub(super) fn parse_owned(document: &str) -> Option<PathBuf> {
    let encoded = document.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let text = unescape(encoded)?;
    if escape(&text) != encoded {
        return None;
    }
    let path = PathBuf::from(&text);
    program_text(&path, Mechanism::LaunchAgent).ok()?;
    Some(path)
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(character),
        }
    }
    out
}

fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        let after = &rest[index + 1..];
        let (replacement, consumed) = [
            ("amp;", '&'),
            ("lt;", '<'),
            ("gt;", '>'),
            ("quot;", '"'),
            ("apos;", '\''),
        ]
        .into_iter()
        .find_map(|(entity, character)| {
            after
                .starts_with(entity)
                .then_some((character, entity.len()))
        })?;
        out.push(replacement);
        rest = &after[consumed..];
    }
    if rest.contains('<') || rest.contains('>') {
        return None;
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_and_parse_roundtrip_special_paths() {
        for text in [
            "/Applications/Totum daemon",
            "/Applications/Totum & Co/dæmon",
            "/tmp/100%/totum-daemon",
            "/tmp/$HOME/totum-daemon",
            "/tmp/\"quoted\"/totum-daemon",
            "/tmp/it's/totum-daemon",
            "/tmp/<tag>/totum-daemon",
        ] {
            let path = PathBuf::from(text);
            let document = document(&path).expect("documento valido");
            assert!(document.contains("RunAtLoad"));
            assert!(!document.contains("KeepAlive"));
            assert!(!document.contains("Program</key>"));
            assert_eq!(parse_owned(&document), Some(path));
        }
    }

    #[test]
    fn foreign_documents_are_not_owned() {
        let path = PathBuf::from("/tmp/a&b/totum-daemon");
        let base = document(&path).unwrap();
        for change in [
            base.replace("app.homun.totum.daemon</string>", "other.label</string>"),
            base.replace("<true/>", "<false/>"),
            base.replace("RunAtLoad", "KeepAlive"),
            base.replace("<array>", "<array>\n\t\t<string>--flag</string>"),
            format!("{base}\n"),
            base.replace("&amp;", "&"),
        ] {
            assert_eq!(parse_owned(&change), None, "{change}");
        }
    }

    #[test]
    fn document_rejects_invalid_programs() {
        for text in [
            "relative/path",
            "/",
            "/tmp/../totum-daemon",
            "/tmp/totum-daemon/",
        ] {
            let result = document(Path::new(text));
            assert!(result.is_err(), "{text}");
        }
    }

    #[test]
    fn file_name_is_provisional_and_consistent() {
        assert_eq!(FILE_NAME, "app.homun.totum.daemon.plist");
    }
}
