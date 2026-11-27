use std::process::Command;

fn daemon() -> Command {
    Command::new(env!("CARGO_BIN_EXE_totum-daemon"))
}

#[test]
fn version_reports_package_version() {
    let output = daemon().arg("--version").output().unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("totum-daemon {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn unsupported_option_is_rejected_with_usage() {
    let output = daemon().arg("--serve").output().unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--serve"));
    assert!(stderr.contains("Uso: totum-daemon"));
}

#[test]
fn version_with_extra_arguments_is_rejected() {
    let output = daemon().args(["--version", "now"]).output().unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("now"));
    assert!(stderr.contains("Uso: totum-daemon"));
}
