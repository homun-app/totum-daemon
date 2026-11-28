use std::ffi::{OsStr, OsString};
use std::process::ExitCode;

use totum_daemon::config::{BackupState, Commit as ConfigCommit, GenerationFailure, StartupKind};
use totum_daemon::identity::Commit as IdentityCommit;
use totum_daemon::journal::{self, Publication};
use totum_daemon::platform;
use totum_daemon::startup::{self, Startup};

const USAGE: &str = "Uso: totum-daemon [--version]";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    if args.is_empty() {
        return run_daemon();
    }

    if args.len() == 1 && args[0].as_os_str() == OsStr::new("--version") {
        println!("totum-daemon {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    reject_args(&args)
}

fn run_daemon() -> ExitCode {
    match start() {
        Ok(startup) => {
            report(&startup);
            eprintln!(
                "totum-daemon {} pronto: radice di stato {}; nessun listener di rete",
                env!("CARGO_PKG_VERSION"),
                startup.root().display()
            );
            // Lo Startup trattiene store, journal, radice e lock per tutta la
            // vita del processo.
            let _startup = startup;
            wait_resident()
        }
        Err(message) => {
            eprintln!("totum-daemon: avvio non riuscito: {message}");
            ExitCode::from(1)
        }
    }
}

fn start() -> Result<Startup, String> {
    let nominal = platform::state_root()
        .map_err(|error| format!("risoluzione della radice di stato fallita: {error}"))?;
    let root = platform::prepare_root(&nominal).map_err(|error| {
        format!(
            "preparazione della radice {} fallita: {error}",
            nominal.display()
        )
    })?;
    let locked = platform::lock(root)
        .map_err(|error| format!("acquisizione del lock di istanza fallita: {error}"))?;
    startup::open(locked).map_err(|error| error.to_string())
}

/// Riporta gli esiti della prima esecuzione, del recupero, della migrazione e
/// della durabilità non confermata. Un commit avvenuto senza durabilità resta
/// un'operazione riuscita e non viene ritentato.
fn report(startup: &Startup) {
    for created in &startup.identity().created {
        eprintln!(
            "totum-daemon: prima esecuzione: creato {}",
            created.file_name()
        );
        if let IdentityCommit::CommittedNotDurable { detail } = created.commit() {
            eprintln!(
                "totum-daemon: {} pubblicato con commit effettuato ma durabilità non confermata: {detail}; non è un errore e non viene ritentato",
                created.file_name()
            );
        }
    }

    if let Some(from) = startup.journal_startup().migrated_from {
        eprintln!(
            "totum-daemon: migrazione del journal: schema {from} trasformato in {}; eventi conservati con la stessa sequenza",
            journal::SCHEMA_VERSION
        );
    }

    match &startup.journal_startup().kind {
        journal::StartupKind::FirstRun { publication } => {
            eprintln!(
                "totum-daemon: prima esecuzione: {} creato",
                journal::JOURNAL_FILE
            );
            report_publication(publication);
        }
        journal::StartupKind::InterruptedBootstrap { publication } => {
            eprintln!(
                "totum-daemon: bootstrap del journal interrotto: {} temporanei scartati, {} creato",
                startup.journal_startup().discarded_temporaries.len(),
                journal::JOURNAL_FILE
            );
            report_publication(publication);
        }
        journal::StartupKind::Existing { repaired } => {
            if *repaired {
                eprintln!(
                    "totum-daemon: journal riaperto dopo un ripristino del motore: stato interno redb ricostruito, dati Totum non reinterpretati"
                );
            }
            if !startup.journal_startup().discarded_temporaries.is_empty() {
                eprintln!(
                    "totum-daemon: {} temporanei residui del journal scartati",
                    startup.journal_startup().discarded_temporaries.len()
                );
            }
        }
    }

    match &startup.config().kind {
        StartupKind::FirstRun { commit } => {
            eprintln!(
                "totum-daemon: prima esecuzione: config.json creato alla revisione {}",
                commit.revision()
            );
            report_config_commit(commit);
        }
        StartupKind::Normal { backup } => {
            if let BackupState::Corrupt { detail } = backup {
                eprintln!(
                    "totum-daemon: config.json.prev inutilizzabile: principale valido e backup non interpretabile ({detail}); il backup non viene riparato"
                );
            }
        }
        StartupKind::Recovered {
            backup,
            main_failure,
            preserved,
            commit,
        } => {
            eprintln!(
                "totum-daemon: recupero: config.json ripristinato dal backup alla revisione {} (principale {})",
                backup.revision(),
                failure_label(main_failure)
            );
            report_preserved(preserved.as_ref());
            report_config_commit(commit);
        }
        StartupKind::Migrated {
            source,
            from_schema_version,
            previous_revision,
            preserved,
            commit,
        } => {
            eprintln!(
                "totum-daemon: migrazione: {} trasformato dallo schema {from_schema_version} alla revisione {} (revisione precedente {previous_revision})",
                source.file_name(),
                commit.revision()
            );
            report_preserved(preserved.as_ref());
            report_config_commit(commit);
        }
    }
}

fn report_publication(publication: &Publication) {
    if let Publication::NotDurable { detail } = publication {
        eprintln!(
            "totum-daemon: {} pubblicato con commit effettuato ma durabilità della voce di directory non confermata: {detail}; non è un errore e non viene ritentato",
            journal::JOURNAL_FILE
        );
    }
}

fn report_config_commit(commit: &ConfigCommit) {
    if let ConfigCommit::CommittedNotDurable { revision, detail } = commit {
        eprintln!(
            "totum-daemon: config.json pubblicato alla revisione {revision} con commit effettuato ma durabilità non confermata: {detail}; non è un errore e non viene ritentato"
        );
    }
}

fn report_preserved(preserved: Option<&std::ffi::OsString>) {
    if let Some(name) = preserved {
        eprintln!(
            "totum-daemon: copia del principale precedente conservata come {}",
            name.to_string_lossy()
        );
    }
}

fn failure_label(failure: &GenerationFailure) -> &'static str {
    match failure {
        GenerationFailure::Absent => "assente",
        GenerationFailure::Corrupt { .. } => "corrotto",
    }
}

/// Blocca il thread principale senza polling e senza standard input.
///
/// Non esistono gestori di segnale: la terminazione è quella predefinita del
/// sistema (segnale su Unix, chiusura della console o del processo su Windows)
/// e il sistema operativo rilascia il lock alla morte del processo. Un risveglio
/// non richiesto riprende l'attesa.
fn wait_resident() -> ExitCode {
    loop {
        std::thread::park();
    }
}

fn reject_args(args: &[OsString]) -> ExitCode {
    let rendered: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let label = if args.len() == 1 {
        "argomento non supportato"
    } else {
        "argomenti non supportati"
    };
    eprintln!("totum-daemon: {label}: {}", rendered.join(" "));
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
