#!/usr/bin/env python3
"""Diagnostica POSIX di SIGTERM: nessun account, login o autostart.

Il modo non privilegiato usa solo la home sintetica del processo. Il modo
root e' riservato a GitHub-hosted e cambia UID soltanto nel processo figlio.
SIGCHLD ignorato e' un controllo negativo: uno stato perso non e' uscita 0.
"""

import argparse
import json
import os
from pathlib import Path
import pwd
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def require_ci_root():
    require(sys.platform in ("darwin", "linux"), "richiesto POSIX supportato")
    require(os.geteuid() == 0, "richiesto root nel solo runner")
    for key, value in (("GITHUB_ACTIONS", "true"),
                       ("RUNNER_ENVIRONMENT", "github-hosted"),
                       ("TOTUM_PROCESS_LIFECYCLE_CI", "1")):
        require(os.environ.get(key) == value, "contesto CI mancante: " + key)


def normalize_child_status():
    # SIG_IGN puo' causare il reap automatico: Popen.wait assume 0 su ECHILD.
    # Il padre deve poter attendere il proprio figlio e leggere lo stato reale.
    signal.signal(signal.SIGCHLD, signal.SIG_DFL)


def strict_wait(child, seconds):
    deadline = time.monotonic() + seconds
    while True:
        try:
            pid, status = os.waitpid(child.pid, os.WNOHANG)
        except ChildProcessError:
            return {"status_available": False, "raw_status": None, "exit_code": None}
        if pid:
            code = os.waitstatus_to_exitcode(status)
            child.returncode = code
            return {"status_available": True, "raw_status": status, "exit_code": code}
        if time.monotonic() >= deadline:
            raise subprocess.TimeoutExpired("waitpid", seconds)
        time.sleep(0.01)


def worker(binary, case, uid):
    privileged = os.geteuid() == 0
    if privileged:
        require_ci_root()
        require(uid >= 20000, "UID del figlio inatteso")
        try:
            pwd.getpwuid(uid)
        except KeyError:
            pass
        else:
            raise RuntimeError("UID gia' associato a un account")
    else:
        require(uid == os.geteuid(), "nessun cambio UID fuori dal runner root")

    # Sotto uno sticky bit il primo componente deve essere dell'UID corrente.
    # Per il figlio con UID diverso si usa un antenato root non condiviso.
    parent = ("/private/var" if sys.platform == "darwin" else "/var/lib") if privileged else None
    base = Path(tempfile.mkdtemp(prefix="totum-process-ci-", dir=parent)).resolve()
    child = None
    result = {"case": case, "parent_euid": os.geteuid(), "child_uid": uid,
              "requested_signal": int(signal.SIGTERM), "fallback_kill_used": False}
    previous_handler = signal.getsignal(signal.SIGCHLD)
    try:
        base.chmod(0o755 if privileged else 0o700)
        home = base / "home"
        home.mkdir(mode=0o700)
        native = base / "totum-daemon"
        shutil.copyfile(binary, native)
        native.chmod(0o755)
        if privileged:
            os.chown(home, uid, uid)

        def demote():
            os.setgroups([])
            os.setgid(uid)
            os.setuid(uid)

        kwargs = {"cwd": str(base), "env": {"PATH": "/usr/bin:/bin", "LANG": "C",
                  "HOME": str(home), "XDG_STATE_HOME": str(home / "state")}}
        if privileged:
            kwargs["preexec_fn"] = demote
        identity = subprocess.run(["/usr/bin/id", "-u"], capture_output=True, text=True,
                                  timeout=5, check=True, **kwargs)
        require(identity.stdout.strip() == str(uid), "UID effettivo del figlio errato")

        if case in ("ignored", "restored"):
            signal.signal(signal.SIGCHLD, signal.SIG_IGN)
        else:
            normalize_child_status()
        result["sigchld_before_normalization"] = int(signal.getsignal(signal.SIGCHLD))
        if case == "restored":
            normalize_child_status()
        result["sigchld_at_spawn"] = int(signal.getsignal(signal.SIGCHLD))

        stderr = base / "daemon.err"
        with stderr.open("wb") as log:
            child = subprocess.Popen([str(native)], stdin=subprocess.DEVNULL,
                                     stdout=subprocess.DEVNULL, stderr=log, **kwargs)
            result["pid"] = child.pid
            deadline = time.monotonic() + 15
            while " pronto:" not in stderr.read_text(errors="replace"):
                observed = child.poll()
                require(observed is None, "daemon terminato prima della prontezza: "
                        + str(observed) + "; " + stderr.read_text(errors="replace"))
                require(time.monotonic() < deadline, "timeout di prontezza")
                time.sleep(0.02)
            result["ready"] = True
            child.send_signal(signal.SIGTERM)
            try:
                result.update(strict_wait(child, 5))
            except subprocess.TimeoutExpired:
                result["fallback_kill_used"] = True
                child.kill()
                result.update(strict_wait(child, 5))
            if not result["status_available"]:
                # Solo diagnosi del vecchio osservatore: MAI accettata come
                # esito del daemon. Il kernel ha gia' eliminato lo stato.
                result["legacy_popen_wait"] = child.wait(timeout=1)

        if case == "ignored":
            require(not result["status_available"], "controllo negativo non riprodotto")
            require(result["legacy_popen_wait"] == 0, "comportamento Popen inatteso")
        else:
            require(result["status_available"] and result["exit_code"] == -signal.SIGTERM,
                    "SIGTERM non osservato: " + json.dumps(result))
        require(not result["fallback_kill_used"], "SIGKILL di ripiego: prova fallita")
        result["passed"] = True
    except Exception as error:
        result["passed"] = False
        result["error"] = str(error)
    finally:
        if child is not None and child.returncode is None:
            # Ripiego esclusivamente di pulizia, mai trasformato in successo.
            try:
                waited = strict_wait(child, 0)
            except subprocess.TimeoutExpired:
                result["cleanup_kill_used"] = True
                child.kill()
                strict_wait(child, 5)
            else:
                if not waited["status_available"]:
                    child.returncode = 0  # solo bookkeeping Popen, stato resta sconosciuto
        signal.signal(signal.SIGCHLD, previous_handler)
        try:
            shutil.rmtree(base)
        except Exception as error:
            result["passed"] = False
            result["cleanup_error"] = str(error)
        result["temporary_tree_removed"] = not base.exists()
    print(json.dumps(result, sort_keys=True), flush=True)
    return 0 if result["passed"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--ci-root", action="store_true")
    parser.add_argument("--worker", choices=("baseline", "ignored", "restored"))
    parser.add_argument("--uid", type=int)
    args = parser.parse_args()
    require(sys.platform in ("darwin", "linux"), "richiesto macOS o Linux")
    if os.geteuid() == 0 or args.ci_root:
        require_ci_root()  # prima di qualunque creazione o lancio del daemon
    binary = args.binary.resolve(strict=True)
    if args.worker:
        require(args.uid is not None, "UID mancante")
        return worker(binary, args.worker, args.uid)

    inherited = signal.getsignal(signal.SIGCHLD)
    normalize_child_status()
    uid = os.geteuid()
    if args.ci_root:
        known = {entry.pw_uid for entry in pwd.getpwall()}
        uid = next(value for value in range(20000, 60000) if value not in known)
    rows = []
    for case in ("baseline", "ignored", "restored"):
        command = [sys.executable, str(Path(__file__).resolve()), str(binary),
                   "--worker", case, "--uid", str(uid)]
        observer = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    text=True, start_new_session=True)
        try:
            stdout, stderr = observer.communicate(timeout=45)
        except subprocess.TimeoutExpired:
            # Gruppo creato solo per questo worker e il suo daemon: nessun
            # processo esterno, nessun nome di account e nessuna ricerca globale.
            try:
                os.killpg(observer.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            observer.communicate(timeout=5)
            raise RuntimeError("timeout worker; gruppo terminato, pulizia filesystem non confermata")
        require(stdout.strip(), "diagnosi assente: " + stderr)
        row = json.loads(stdout)
        rows.append(row)
        print(json.dumps(row, sort_keys=True), flush=True)
        require(observer.returncode == 0 and row.get("passed"), "prova fallita: " + case)
    print(json.dumps({"passed": True, "cases": len(rows),
                      "inherited_sigchld": int(inherited) if isinstance(inherited, int)
                      else "custom_or_unknown", "accounts_created": 0}), flush=True)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as failure:
        print(json.dumps({"passed": False, "error": str(failure)}), flush=True)
        sys.exit(1)
