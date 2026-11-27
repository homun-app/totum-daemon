//! Coordinazione dei soli test fra fork/exec e rilascio-riacquisizione dei lock.
//!
//! Il descrittore ereditato da un figlio puo' trattenere il lock fino a exec,
//! anche dopo la chiusura nel padre. Tutti gli avvii di figli della libreria
//! partecipano allo stesso mutex delle transizioni di config. La guardia
//! termina appena spawn ritorna; non copre l'attesa del processo figlio.

use std::io;
use std::process::{Child, Command};
use std::sync::{Mutex, MutexGuard, PoisonError};

static FORKS: Mutex<()> = Mutex::new(());

pub(crate) fn fork_window() -> MutexGuard<'static, ()> {
    FORKS.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn spawn(command: &mut Command) -> io::Result<Child> {
    let _forks = fork_window();
    command.spawn()
}

#[test]
fn child_spawn_waits_for_the_lock_transition() {
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

    let transition = fork_window();
    let (started_tx, started_rx) = mpsc::channel();
    let (spawned_tx, spawned_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut command = Command::new(std::env::current_exe().expect("binario di prova"));
        command
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        started_tx.send(()).expect("segnale di avvio");
        let mut child = spawn(&mut command).expect("figlio avviabile");
        spawned_tx.send(()).expect("segnale di spawn");
        assert!(child.wait().expect("figlio terminato").success());
    });
    started_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("worker avviato");
    let blocked = matches!(
        spawned_rx.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    drop(transition);
    worker.join().expect("worker terminato");
    assert!(
        blocked,
        "il figlio non deve ereditare lock durante una transizione"
    );
}
