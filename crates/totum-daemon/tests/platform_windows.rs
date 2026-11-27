//! Prove d'integrazione del contratto di apertura su Windows.
//!
//! Le prove sono compilate per `x86_64-pc-windows-gnu` ma non sono state
//! eseguite in questa fase: richiedono una macchina Windows con un utente non
//! elevato. Usano soltanto directory temporanee sotto `target/platform-tests`.

#![cfg(windows)]

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use totum_daemon::platform::{self, PlatformError};

static NEXT: AtomicU32 = AtomicU32::new(0);

fn scratch(label: &str) -> PathBuf {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("radice del workspace")
        .join("target/platform-tests");
    fs::create_dir_all(&base).expect("directory base creabile");
    let path = base.join(format!(
        "{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir(&path).expect("directory di prova creabile");
    path
}

#[test]
fn absent_file_is_reported_as_absent_not_as_a_changed_path() {
    let base = scratch("win-absent");
    let root = platform::prepare_root(&base.join("Totum")).expect("radice preparabile");
    root.verify_path().expect("radice ancora coerente");

    match root.open_file(OsStr::new("config.json")) {
        Err(PlatformError::FileAbsent { path }) => {
            assert_eq!(path, root.path().join("config.json"))
        }
        other => panic!("esito inatteso: {other:?}"),
    }
}

#[test]
fn present_file_is_reopened_with_its_contents() {
    let base = scratch("win-roundtrip");
    let root = platform::prepare_root(&base.join("Totum")).expect("radice preparabile");

    let mut file = root
        .create_file(OsStr::new("config.json"))
        .expect("file creabile");
    file.write_all(b"{\"schemaVersion\":1,\"revision\":0}")
        .expect("contenuto scritto");
    drop(file);

    let mut reopened = root
        .open_file(OsStr::new("config.json"))
        .expect("file riapribile");
    let mut content = String::new();
    reopened
        .read_to_string(&mut content)
        .expect("contenuto leggibile");
    assert_eq!(content, "{\"schemaVersion\":1,\"revision\":0}");
}

#[test]
fn renamed_or_replaced_root_is_refused_and_never_reported_absent() {
    let base = scratch("win-moved");
    let nominal = base.join("Totum");
    let root = platform::prepare_root(&nominal).expect("radice preparabile");

    let moved = base.join("moved");
    fs::rename(&nominal, &moved).expect("radice rinominata");
    fs::create_dir(&nominal).expect("radice sostituita");

    assert!(matches!(
        root.verify_path(),
        Err(PlatformError::PathChanged { .. })
    ));
    match root.open_file(OsStr::new("config.json")) {
        Err(PlatformError::PathChanged { .. }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
}

#[test]
fn removed_root_is_a_changed_path_not_an_absent_file() {
    let base = scratch("win-removed");
    let nominal = base.join("Totum");
    let root = platform::prepare_root(&nominal).expect("radice preparabile");
    fs::remove_dir(&nominal).expect("radice rimossa");

    match root.open_file(OsStr::new("config.json")) {
        Err(PlatformError::PathChanged { .. }) => {}
        other => panic!("esito inatteso: {other:?}"),
    }
}

#[test]
fn directory_in_place_of_a_file_is_refused() {
    let base = scratch("win-directory");
    let root = platform::prepare_root(&base.join("Totum")).expect("radice preparabile");
    fs::create_dir(root.path().join("config.json")).expect("directory creata");

    match root.open_file(OsStr::new("config.json")) {
        Err(error) => assert!(
            !matches!(
                error,
                PlatformError::FileAbsent { .. } | PlatformError::PathChanged { .. }
            ),
            "una directory non può diventare assenza o cambio di percorso: {error:?}"
        ),
        Ok(_) => panic!("una directory non deve essere aperta come file"),
    }
}
