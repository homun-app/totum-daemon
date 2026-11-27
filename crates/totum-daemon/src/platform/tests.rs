//! Test di libreria del modulo `platform` sul sistema Unix corrente.
//!
//! Le prove che richiedono un secondo account o un altro sistema non sono qui:
//! sono descritte nella documentazione e dichiarate non eseguite.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use super::{PlatformError, validate_nominal, validate_single_component};

static NEXT: AtomicU32 = AtomicU32::new(0);

fn scratch(label: &str) -> PathBuf {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("radice del workspace")
        .join("target/platform-tests");
    fs::create_dir_all(&base).expect("directory di prova creabile");
    let path = base.join(format!(
        "{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir(&path).expect("radice di prova creabile");
    path
}

#[test]
fn nominal_must_be_absolute() {
    assert!(matches!(
        validate_nominal(Path::new("totum")),
        Err(PlatformError::InvalidPath { .. })
    ));
}

#[test]
fn nominal_rejects_dot_components_and_filesystem_root() {
    for path in ["/tmp/../etc/totum", "/tmp/./totum", "/"] {
        assert!(
            matches!(
                validate_nominal(Path::new(path)),
                Err(PlatformError::InvalidPath { .. })
            ),
            "{path} doveva essere rifiutato"
        );
    }
    assert!(validate_nominal(Path::new("/tmp/totum")).is_ok());
}

#[test]
fn file_names_must_be_one_component() {
    assert!(validate_single_component(std::ffi::OsStr::new("config.json")).is_ok());
    for name in ["", ".", "..", "a/b", "/abs"] {
        assert!(
            validate_single_component(std::ffi::OsStr::new(name)).is_err(),
            "{name:?} doveva essere rifiutato"
        );
    }
}

#[test]
fn is_elevated_matches_the_effective_identifier() {
    let elevated = super::is_elevated().expect("rilevamento disponibile");
    assert_eq!(elevated, super::euid() == 0);
}

#[test]
fn the_lock_is_released_only_when_the_last_reference_is_closed() {
    let base = scratch("last-reference");
    let directory = base.join("totum");

    let held = super::lock(super::prepare_root(&directory).expect("radice preparabile"))
        .expect("prima acquisizione");
    // `try_clone` produce un secondo riferimento alla stessa open file
    // description, come un processo forchettato prima di `exec`: finché resta
    // aperto, chiudere il descrittore originale non rilascia il lock. È la
    // ragione per cui l'harness coordina l'avvio dei figli con le transizioni
    // del lock.
    let duplicate = held._file.try_clone().expect("descrittore duplicato");
    drop(held);

    match super::lock(super::prepare_root(&directory).expect("radice preparabile")) {
        Err(PlatformError::AlreadyRunning { .. }) => {}
        other => panic!("il lock è stato rilasciato con un riferimento aperto: {other:?}"),
    }

    drop(duplicate);
    let released = super::lock(super::prepare_root(&directory).expect("radice preparabile"))
        .expect("la chiusura dell'ultimo riferimento rilascia il lock");
    drop(released);
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn elevated_execution_is_refused_before_any_creation() {
    let base = scratch("elevation");
    let nominal = base.join("state");

    let error = super::unix::prepare_root_with(&nominal, || Ok(true))
        .expect_err("l'esecuzione elevata deve essere rifiutata");

    assert!(
        matches!(error, PlatformError::ElevatedExecution),
        "errore inatteso: {error:?}"
    );
    assert!(!nominal.exists(), "nessun componente andava creato");

    let _ = fs::remove_dir_all(&base);
}
