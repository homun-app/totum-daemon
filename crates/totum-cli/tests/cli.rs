use std::process::Command;

fn totum() -> Command {
    Command::new(env!("CARGO_BIN_EXE_totum"))
}

#[test]
fn version_reports_cli_version() {
    let output = totum().arg("--version").output().unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("totum {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn status_reports_connection_not_implemented() {
    let output = totum().arg("status").output().unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("totum "));
    assert!(stdout.contains("Collegamento al daemon: non implementato"));
    assert!(stdout.contains("Nessun controllo di raggiungibilità"));
    assert!(output.stderr.is_empty());
}

#[test]
fn missing_command_is_rejected_with_usage() {
    let output = totum().output().unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("comando mancante"));
    assert!(stderr.contains("Uso: totum"));
}

#[test]
fn unknown_command_is_rejected() {
    let output = totum().arg("pair").output().unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("comando non supportato: pair"));
    assert!(stderr.contains("Uso: totum"));
}

#[test]
fn status_with_extra_arguments_is_rejected() {
    let output = totum().args(["status", "--json"]).output().unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("status"));
    assert!(stderr.contains("--json"));
    assert!(stderr.contains("Uso: totum"));
}
