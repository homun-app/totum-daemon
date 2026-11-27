use std::ffi::{OsStr, OsString};
use std::process::ExitCode;

const USAGE: &str = "Uso: totum <status | --version>";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    if args.is_empty() {
        eprintln!("totum: comando mancante");
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    if args.len() == 1 {
        if args[0].as_os_str() == OsStr::new("--version") {
            println!("totum {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        if args[0].as_os_str() == OsStr::new("status") {
            return report_status();
        }
    }

    reject(&args)
}

fn report_status() -> ExitCode {
    println!("totum {}", env!("CARGO_PKG_VERSION"));
    println!("Collegamento al daemon: non implementato.");
    println!("Nessun controllo di raggiungibilità è eseguito in questo scaffold.");
    ExitCode::SUCCESS
}

fn reject(args: &[OsString]) -> ExitCode {
    let command = args[0].to_string_lossy();
    let known = args[0].as_os_str() == OsStr::new("status")
        || args[0].as_os_str() == OsStr::new("--version");

    if known {
        let extras: Vec<String> = args[1..]
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        eprintln!(
            "totum: argomenti non supportati per \"{command}\": {}",
            extras.join(" ")
        );
    } else {
        eprintln!("totum: comando non supportato: {command}");
    }
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
