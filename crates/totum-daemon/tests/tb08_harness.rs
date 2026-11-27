//! Prove non privilegiate dell'harness TB-08 (D3 P10-A).
//!
//! L'harness privilegiato crea account reali e va eseguito solo in un runner CI
//! Linux effimero. Questo file non lo invoca mai con privilegi: esegue il suo
//! `--self-test`, che ripete i controlli d'ambiente e valuta i verdetti con
//! comandi simulati, e verifica che fuori dal contesto dichiarato l'harness si
//! fermi prima di qualsiasi modifica.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn harness() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/scripts/tb08-linux.sh")
}

fn bash() -> PathBuf {
    for candidate in ["/bin/bash", "/usr/bin/bash"] {
        if Path::new(candidate).exists() {
            return PathBuf::from(candidate);
        }
    }
    panic!("bash non disponibile: l'harness TB-08 non è verificabile");
}

/// Directory con uno stub di `uname` che finge Linux e stub che registrano le
/// invocazioni degli strumenti capaci di modificare la macchina.
struct Scratch {
    dir: PathBuf,
    stubs: PathBuf,
    log: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir().join(format!("totum-tb08-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("directory di prova creabile");
        let stubs = base.join("stubs");
        fs::create_dir_all(&stubs).expect("directory stub creabile");
        let log = base.join("mutations.log");
        fs::write(&log, b"").expect("registro creabile");
        fs::write(stubs.join("uname"), "#!/bin/sh\nprintf 'Linux\\n'\n").expect("stub scrivibile");
        for tool in ["useradd", "userdel", "pkill", "setpriv", "runuser"] {
            let body = format!(
                "#!/bin/sh\nprintf '{tool} %s\\n' \"$*\" >> \"{log}\"\nexit 0\n",
                log = log.display()
            );
            fs::write(stubs.join(tool), body).expect("stub scrivibile");
        }
        for entry in fs::read_dir(&stubs).expect("stub leggibile") {
            let entry = entry.expect("voce di stub");
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o755))
                .expect("stub eseguibile");
        }
        Self {
            dir: base,
            stubs,
            log,
        }
    }

    fn env<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        let mut path = self.stubs.display().to_string();
        path.push(':');
        path.push_str(&std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string()));
        command
            .env("PATH", path)
            .env("TB08_STUB_LOG", &self.log)
            .env_remove("TOTUM_TB08_CI")
            .env_remove("CI")
            .env_remove("GITHUB_ACTIONS")
            .env_remove("RUNNER_ENVIRONMENT")
            .env_remove("TB08_SELFTEST_PROBE")
    }

    fn mutations(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn run(scratch: &Scratch, args: &[&str]) -> Output {
    let mut command = Command::new(bash());
    let harness = harness();
    let output = scratch
        .env(&mut command)
        .arg(harness)
        .args(args)
        .output()
        .expect("harness eseguibile");
    assert!(
        output.status.code().is_some(),
        "l'harness è terminato da un segnale"
    );
    output
}

#[test]
fn harness_self_test_passes_without_privileges() {
    let output = Command::new(bash())
        .arg(harness())
        .arg("--self-test")
        .output()
        .expect("self-test eseguibile");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "self-test TB-08 fallito:\n{text}");
    assert!(
        text.contains("nessun account reale verrà creato"),
        "manca la dichiarazione di assenza di account reali:\n{text}"
    );
    assert!(
        !text.contains("FAIL:"),
        "nessuno scenario deve fallire:\n{text}"
    );
}

#[test]
fn harness_refuses_without_declared_ci_context() {
    let scratch = Scratch::new("noci");
    let output = run(&scratch, &["/bin/echo"]);
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(2),
        "senza contesto CI l'harness deve uscire per ambiente inadeguato:\n{text}"
    );
    assert!(
        text.contains("TOTUM_TB08_CI"),
        "manca la diagnosi sul contesto CI:\n{text}"
    );
    assert!(
        !text.contains("richiede root"),
        "il vincolo CI deve precedere quello sui privilegi: il solo root non basta\n{text}"
    );
    assert!(
        scratch.mutations().is_empty(),
        "nessuno strumento di modifica invocato: {}",
        scratch.mutations()
    );
}

#[test]
fn harness_refuses_an_ephemerality_that_is_not_declared() {
    let cases: [(&str, &[(&str, &str)]); 2] = [
        (
            "runner-self-hosted",
            &[
                ("TOTUM_TB08_CI", "1"),
                ("CI", "true"),
                ("RUNNER_ENVIRONMENT", ""),
            ],
        ),
        (
            "contesto-non-dichiarato",
            &[("TOTUM_TB08_CI", "1"), ("CI", ""), ("GITHUB_ACTIONS", "")],
        ),
    ];
    for (label, vars) in cases {
        let scratch = Scratch::new(label);
        let mut command = Command::new(bash());
        scratch.env(&mut command);
        for (key, value) in vars {
            command.env(key, value);
        }
        let output = command
            .arg(harness())
            .arg("/bin/echo")
            .output()
            .expect("harness eseguibile");
        let text = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(
            output.status.code(),
            Some(2),
            "ambiente {label} deve essere rifiutato:\n{text}"
        );
        assert!(
            scratch.mutations().is_empty(),
            "ambiente {label}: nessuna modifica tentata: {}",
            scratch.mutations()
        );
    }
}

#[test]
fn probe_mode_stops_before_any_mutation() {
    let scratch = Scratch::new("probe");
    let mut command = Command::new(bash());
    scratch.env(&mut command);
    command
        .env("TOTUM_TB08_CI", "1")
        .env("GITHUB_ACTIONS", "true")
        .env("CI", "true")
        .env("RUNNER_ENVIRONMENT", "github-hosted")
        .env("TB08_SELFTEST_PROBE", "1");
    let output = command
        .arg(harness())
        .arg("/bin/echo")
        .output()
        .expect("harness eseguibile");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let mutations = scratch.mutations();
    assert!(
        mutations.is_empty(),
        "la sonda non invoca strumenti di modifica: {mutations}"
    );
    if output.status.success() {
        assert!(
            stdout.contains("arrestata prima"),
            "l'esito privilegiato deve dichiarare l'arresto: {stdout}"
        );
    } else {
        assert_eq!(
            output.status.code(),
            Some(2),
            "l'esito non privilegiato della sonda deve essere 2: {stderr}"
        );
        assert!(
            stderr.contains("richiede root"),
            "la sonda non privilegiata deve fermarsi sui privilegi: {stderr}"
        );
    }
}
