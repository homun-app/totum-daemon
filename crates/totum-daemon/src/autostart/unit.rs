//! Codifica e riconoscimento dell'unità systemd utente.
//!
//! `ExecStart` contiene un solo argomento, sempre racchiuso fra virgolette
//! doppie, preceduto dal prefisso `:` documentato da `systemd.service(5)`.
//! Il prefisso disabilita la sostituzione delle variabili d'ambiente: un
//! percorso contenente `$HOME`, `${HOME}` o `$$` resta letterale e non viene
//! espanso. I specificatori `%` vengono invece espansi anche con il prefisso,
//! quindi un percentuale letterale resta codificato come `%%`. La doppia barra
//! rovesciata e la virgoletta protetta completano le regole di quoting. Il
//! documento non contiene altri argomenti né variabili.

use std::path::{Path, PathBuf};

use super::{AutostartError, Mechanism, program_text};

/// Nome dell'unità nella directory utente.
pub(super) const FILE_NAME: &str = "totum-daemon.service";
/// Directory dei collegamenti creata da `systemd` con `WantedBy`.
pub(super) const WANTS_DIR: &str = "default.target.wants";
/// Contenuto relativo del collegamento, valido dalla directory `wants`.
pub(super) const LINK_TARGET: &str = "../totum-daemon.service";

const PREFIX: &str = "[Unit]\n\
Description=Totum daemon\n\
\n\
[Service]\n\
ExecStart=:";

const SUFFIX: &str = "\n\
\n\
[Install]\n\
WantedBy=default.target\n";

/// Costruisce il documento per il percorso indicato.
pub(super) fn document(program: &Path) -> Result<String, AutostartError> {
    let text = program_text(program, Mechanism::SystemdUser)?;
    Ok(format!("{PREFIX}{}{SUFFIX}", quote(&text)))
}

/// Riconosce il percorso di un documento prodotto da [`document`].
pub(super) fn parse_owned(document: &str) -> Option<PathBuf> {
    let encoded = document.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let text = unquote(encoded)?;
    if quote(&text) != encoded {
        return None;
    }
    let path = PathBuf::from(&text);
    program_text(&path, Mechanism::SystemdUser).ok()?;
    Some(path)
}

/// Indica se il testo di un collegamento è una delle forme canoniche che
/// systemd risolve verso la nostra unità.
///
/// Non segue il collegamento e non risolve componenti: confronta il testo con
/// la forma relativa prevista o con il percorso assoluto dell'unità.
pub(super) fn is_owned_link_target(target: &Path, unit_path: &Path) -> bool {
    target == Path::new(LINK_TARGET) || target == unit_path
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            _ => out.push(character),
        }
    }
    out.push('"');
    out
}

fn unquote(encoded: &str) -> Option<String> {
    let inner = encoded.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => out.push(match characters.next()? {
                '\\' => '\\',
                '"' => '"',
                _ => return None,
            }),
            '%' => {
                if characters.next()? != '%' {
                    return None;
                }
                out.push('%');
            }
            other => out.push(other),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_and_parse_roundtrip_special_paths() {
        for text in [
            "/opt/Totum daemon",
            "/opt/Totum & Co/dæmon",
            "/tmp/100%/totum-daemon",
            "/tmp/$HOME/totum-daemon",
            "/tmp/\"quoted\"/totum-daemon",
            "/tmp/back\\slash/totum-daemon",
            "/tmp/a;b/totum-daemon",
        ] {
            let path = PathBuf::from(text);
            let document = document(&path).expect("documento valido");
            assert!(document.contains("WantedBy=default.target"));
            assert!(!document.contains("systemctl"));
            assert_eq!(parse_owned(&document), Some(path));
        }
    }

    #[test]
    fn foreign_documents_are_not_owned() {
        let base = document(Path::new("/tmp/100%/totum-daemon")).unwrap();
        for change in [
            base.replace("Totum daemon", "Other"),
            base.replace("WantedBy=default.target", "WantedBy=multi-user.target"),
            base.replace("ExecStart=:", "ExecStart="),
            base.replace("ExecStart=:\"", "ExecStart=:--flag \""),
            format!("{base}\n"),
            base.replace("%%", "%"),
        ] {
            assert_eq!(parse_owned(&change), None, "{change}");
        }
    }

    #[test]
    fn quoted_encoding_matches_systemd_rules() {
        assert_eq!(
            quote("/tmp/100%/$HOME/a\\b\"c"),
            "\"/tmp/100%%/$HOME/a\\\\b\\\"c\""
        );
        assert_eq!(
            unquote("\"/tmp/100%%/$HOME/a\\\\b\\\"c\"").as_deref(),
            Some("/tmp/100%/$HOME/a\\b\"c")
        );
    }

    #[test]
    fn dollar_forms_remain_literal_with_the_no_expansion_prefix() {
        for (text, expected_exec) in [
            (
                "/opt/$HOME/totum-daemon",
                "ExecStart=:\"/opt/$HOME/totum-daemon\"",
            ),
            (
                "/opt/${HOME}/totum-daemon",
                "ExecStart=:\"/opt/${HOME}/totum-daemon\"",
            ),
            (
                "/opt/$$/totum-daemon",
                "ExecStart=:\"/opt/$$/totum-daemon\"",
            ),
            (
                "/opt/a$b%c d/totum-daemon",
                "ExecStart=:\"/opt/a$b%%c d/totum-daemon\"",
            ),
            (
                "/opt/$HOME/%s/\"q\"/back\\slash/totum-daemon",
                "ExecStart=:\"/opt/$HOME/%%s/\\\"q\\\"/back\\\\slash/totum-daemon\"",
            ),
        ] {
            let document = document(Path::new(text)).expect("documento valido");
            assert!(
                document.contains(expected_exec),
                "ExecStart inatteso: {document}"
            );
            assert_eq!(parse_owned(&document), Some(PathBuf::from(text)));
        }
    }

    #[test]
    fn the_consumer_receives_the_requested_literal_argument() {
        // La forma attesa è quella documentata: `:` disabilita la sostituzione
        // delle variabili, mentre `%` resta uno specificatore e viene protetto
        // con `%%`. Il test confronta il documento con una forma scritta a mano,
        // non soltanto con l'inversione del nostro encoder.
        let manual = format!("{PREFIX}\"/opt/$HOME/${{HOME}}/100%%/a\\\\b\\\"c\"{SUFFIX}");
        assert!(manual.contains("ExecStart=:\"/opt/$HOME/${HOME}/100%%/"));
        assert_eq!(
            parse_owned(&manual),
            Some(PathBuf::from("/opt/$HOME/${HOME}/100%/a\\b\"c"))
        );

        let without_prefix = manual.replacen("ExecStart=:", "ExecStart=", 1);
        assert_eq!(parse_owned(&without_prefix), None);
    }

    #[test]
    fn link_target_is_recognized_without_following() {
        let unit = Path::new("/home/u/.config/systemd/user/totum-daemon.service");
        assert!(is_owned_link_target(Path::new(LINK_TARGET), unit));
        assert!(is_owned_link_target(unit, unit));
        assert!(!is_owned_link_target(Path::new("../other.service"), unit));
        assert!(!is_owned_link_target(
            Path::new("/tmp/default.target.wants/totum-daemon.service"),
            unit
        ));
    }
}
