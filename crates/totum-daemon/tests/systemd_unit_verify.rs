//! Verifica del formato systemd con il consumatore reale (D3 P10-A).
//!
//! Il modulo applicativo non esegue strumenti di sistema. Questa prova, che
//! vive nell'harness Unix e si salta fuori da Linux, installa un'unità utente
//! in una directory temporanea con un percorso del binario contenente spazi,
//! percentuali e dollari, e la sottopone a `systemd-analyze verify` quando lo
//! strumento è disponibile. Nessun servizio viene caricato, abilitato o
//! avviato: la verifica è di sola analisi del documento. In CI
//! `TOTUM_REQUIRE_SYSTEMD_ANALYZE=1` rende l'assenza dello strumento un
//! fallimento invece di una prova saltata; sui sistemi senza systemd (per
//! esempio macOS) la prova si salta con un messaggio esplicito.
//!
//! Le prove usano soltanto directory temporanee e non toccano
//! `~/.config/systemd/user` né la sessione dell'utente.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use totum_daemon::autostart::{Autostart, Layout, RemoveChange, Status};

static NEXT: AtomicU32 = AtomicU32::new(0);

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "totum-systemd-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("directory di prova creabile");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("permessi della directory di prova");
        Self { path }
    }

    fn join(&self, relative: &str) -> PathBuf {
        self.path.join(relative)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn systemd_analyze() -> Option<PathBuf> {
    let candidate = PathBuf::from("systemd-analyze");
    let output = Command::new(&candidate).arg("--version").output().ok()?;
    output.status.success().then_some(candidate)
}

fn require_or_skip() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        if std::env::var_os("TOTUM_REQUIRE_SYSTEMD_ANALYZE").is_some() {
            panic!("TOTUM_REQUIRE_SYSTEMD_ANALYZE richiede Linux");
        }
        eprintln!("verifica systemd: piattaforma diversa da Linux, saltata");
        return None;
    }
    match systemd_analyze() {
        Some(path) => Some(path),
        None if std::env::var_os("TOTUM_REQUIRE_SYSTEMD_ANALYZE").is_some() => {
            panic!("TOTUM_REQUIRE_SYSTEMD_ANALYZE è impostata ma systemd-analyze non è disponibile")
        }
        None => {
            eprintln!("systemd-analyze non disponibile: verifica del consumatore reale saltata");
            None
        }
    }
}

fn write_executable(path: &Path) {
    fs::create_dir_all(path.parent().expect("directory del programma"))
        .expect("directory creabile");
    fs::write(path, "#!/bin/sh\nexit 0\n").expect("programma di prova scritto");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .expect("programma di prova eseguibile");
}

/// Installa l'unità che punta a `binary` e ne restituisce il percorso: la
/// codifica canonica è la stessa per un programma esistente e per uno assente,
/// quindi le due prove condividono l'unica codifica in produzione.
fn install_unit(scratch: &Scratch, binary: &Path) -> (Autostart, PathBuf) {
    let autostart = Autostart::new(Layout::systemd_user(scratch.join("systemd/user"), true));
    assert_eq!(autostart.status().expect("stato iniziale"), Status::Absent);
    let report = autostart
        .install(binary)
        .expect("unità installabile con un percorso assoluto");
    assert_eq!(
        report.entry,
        totum_daemon::autostart::InstallChange::Created
    );
    assert_eq!(
        autostart.status().expect("stato dopo la registrazione"),
        Status::Registered {
            program: binary.to_path_buf()
        }
    );
    let unit = scratch.join("systemd/user/totum-daemon.service");
    let document = fs::read_to_string(&unit).expect("unità leggibile");
    assert!(
        document.contains("100%%"),
        "la percentuale letterale resta protetta:\n{document}"
    );
    assert!(
        document.contains("$HOME"),
        "il dollaro resta letterale con il prefisso ':' :\n{document}"
    );
    (autostart, unit)
}

fn verify(analyze: &Path, unit: &Path) -> std::process::Output {
    Command::new(analyze)
        .args(["--man=no", "verify"])
        .arg(unit)
        .output()
        .expect("systemd-analyze eseguibile")
}

#[test]
fn systemd_analyze_accepts_the_unit_with_special_paths() {
    let analyze = match require_or_skip() {
        Some(path) => path,
        None => return,
    };

    let scratch = Scratch::new();
    let binary = scratch.join("Totum 100% $HOME/dæmon/totum-daemon");
    write_executable(&binary);

    let (autostart, unit) = install_unit(&scratch, &binary);
    let document = fs::read_to_string(&unit).expect("unità leggibile");
    assert!(
        document.contains("Totum 100%% $HOME/dæmon/totum-daemon"),
        "il percorso richiesto è quello del programma:\n{document}"
    );

    let output = verify(&analyze, &unit);
    assert!(
        output.status.success(),
        "systemd-analyze verify ha rifiutato l'unità: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        autostart.remove().expect("rimozione"),
        totum_daemon::autostart::RemoveReport {
            entry: RemoveChange::Removed,
            link: Some(RemoveChange::Removed),
            durability: totum_daemon::autostart::Durability::Confirmed,
        }
    );
    assert_eq!(autostart.status().expect("stato finale"), Status::Absent);
    assert!(
        !scratch.join("systemd/user/totum-daemon.service").exists(),
        "l'unità rimossa non resta"
    );
}

/// Il consumatore reale rifiuta un'unità il cui eseguibile non esiste: una
/// prova che passasse comunque starebbe verificando un documento che il
/// sistema non userebbe mai.
#[test]
fn systemd_analyze_rejects_a_unit_pointing_at_a_missing_program() {
    let analyze = match require_or_skip() {
        Some(path) => path,
        None => return,
    };

    let scratch = Scratch::new();
    let binary = scratch.join("Totum 100% $HOME/assente/totum-daemon");
    assert!(
        !binary.try_exists().expect("controllo del percorso assente"),
        "il programma di questa prova non deve esistere: {}",
        binary.display()
    );

    let (_autostart, unit) = install_unit(&scratch, &binary);
    let output = verify(&analyze, &unit);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "un'esecuzione mancante deve essere segnalata dal consumatore reale:\n{stderr}"
    );
    assert!(
        stderr.contains("totum-daemon"),
        "la segnalazione nomina il programma mancante:\n{stderr}"
    );
    eprintln!(
        "verifica systemd: eseguibile mancante rifiutato come atteso: {}",
        stderr.trim()
    );
}
