#!/usr/bin/env bash
# TB-08: isolamento della radice di stato fra due account (D3 P10-A).
#
# Esecuzione privilegiata, ammessa soltanto in un runner CI Linux effimero e
# GitHub-hosted:
#
#   sudo env TOTUM_TB08_CI=1 GITHUB_ACTIONS="$GITHUB_ACTIONS" \
#        RUNNER_ENVIRONMENT="$RUNNER_ENVIRONMENT" \
#        .github/scripts/tb08-linux.sh /percorso/totum-daemon
#
# Crea due account effimeri, avvia il daemon con il primo e verifica che il
# secondo non possa leggere, rinominare, sostituire o aprire gli oggetti dello
# stato. Viene rimosso soltanto ciò che questa esecuzione ha creato e
# registrato: un account preesistente non viene terminato né eliminato. Essere
# root non basta: Linux, il contesto CI dichiarato e la conferma esplicita
# TOTUM_TB08_CI=1 sono verificati prima di qualunque modifica.
#
# Esecuzione non privilegiata, senza account reali:
#
#   .github/scripts/tb08-linux.sh --self-test
#
# Il self-test ripete i controlli d'ambiente e valuta i verdetti con comandi
# simulati: collisione iniziale, errore creando il secondo account, esecuzione
# fuori CI, cleanup ripetuta, diniego per singolo strumento e uscite inattese
# del secondo daemon. Le prove sui segnali usano sottoprocessi con gli stessi
# stub: la modalità interna --signal-trial si rifiuta di partire se useradd non
# si risolve nella directory degli stub, quindi non può creare account reali.
# Con TB08_SELFTEST_PROBE=1 l'harness si ferma subito dopo i controlli
# d'ambiente, quindi neppure una esecuzione privilegiata può creare account.
#
# Esce 0 se tutte le verifiche passano, 1 su una verifica fallita, 2 su uso
# scorretto o ambiente inadeguato, 129/130/143 se interrotto da HUP, INT o TERM.

set -Eeuo pipefail
IFS=$'\n\t'

SCRIPT_PATH="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
PROBE=${TB08_SELFTEST_PROBE:-}

BINARY=""
WORK=""
BINDIR=""
STUBS=""
STUBS_DARWIN=""
DB=""
LOG=""
TOTAL=""
DAEMON_BIN=""
DAEMON_PID=""
USER_A=""
USER_B=""
UID_A=""
GID_A=""
UID_B=""
HOME_A=""
HOME_B=""
STATE=""
STATE_PARENT=""
CONTROL_PARENT=""
OWN_TREE=""
DECOY=""
CLEANED=0
declare -a CREATED_ACCOUNTS=()
declare -a RUN_AS_PREFIX=()

say() { printf '%s\n' "$*"; }
pass() { printf 'PASS: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
die_env() { printf 'AMBIENTE INADEGUATO: %s\n' "$*" >&2; exit 2; }

usage() {
  printf "uso: %s /percorso/totum-daemon\n" "$0" >&2
  printf "uso: %s --self-test\n" "$0" >&2
}

# ---------------------------------------------------------------------------
# Controlli d'ambiente. Nessuno di questi modifica il sistema e l'ordine è
# parte della prova: il contesto CI è richiesto indipendentemente dai privilegi.
# ---------------------------------------------------------------------------

require_linux() {
  local os
  os=$(uname -s 2>/dev/null || printf 'sconosciuto')
  if [[ $os != Linux ]]; then
    die_env "TB-08 richiede Linux: l'isolamento fra account è provato solo lì. Rilevato: $os"
  fi
}

require_ci_context() {
  if [[ ${TOTUM_TB08_CI:-} != 1 ]]; then
    die_env "TOTUM_TB08_CI=1 assente: questa prova crea account ed è ammessa solo in un ambiente effimero dichiarato"
  fi
  if [[ ${GITHUB_ACTIONS:-} != true && ${CI:-} != true ]]; then
    die_env "contesto CI non dichiarato a chi esegue: GITHUB_ACTIONS=true o CI=true devono attraversare sudo, il solo root non basta"
  fi
  if [[ ${RUNNER_ENVIRONMENT:-} != github-hosted ]]; then
    die_env "runner non dichiarato GitHub-hosted (RUNNER_ENVIRONMENT='${RUNNER_ENVIRONMENT:-non impostata}'): l'isolamento effimero non è garantito"
  fi
}

require_root() {
  if [[ $EUID -ne 0 ]]; then
    die_env "TB-08 richiede root per creare e rimuovere gli account effimeri"
  fi
}

require_tools() {
  local tool
  for tool in useradd userdel setpriv runuser timeout stat find sha256sum install \
    mktemp pkill id uname sed grep cut sort xargs ls cat touch ln mv rm mkdir chmod seq; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      die_env "strumento richiesto non disponibile: $tool"
    fi
  done
}

# ---------------------------------------------------------------------------
# Registro degli account creati da questa esecuzione e cleanup idempotente.
# ---------------------------------------------------------------------------

account_exists() { id -- "$1" >/dev/null 2>&1; }

account_uid() { id -u -- "$1" 2>/dev/null; }

account_gid() { id -g -- "$1" 2>/dev/null; }

# Crea un account effimero e lo registra soltanto se è stato creato ora. Un
# account già presente è un ambiente inadeguato: non viene toccato né rimosso.
create_account() { # $1 nome, $2 home
  local name=$1 home=$2 uid
  if account_exists "$name"; then
    printf "l'account %s esisteva già prima di questa esecuzione: non viene creato né rimosso\n" "$name" >&2
    return 1
  fi
  if ! useradd --create-home --home-dir "$home" --shell /bin/bash "$name"; then
    printf "creazione dell'account %s fallita: nessun oggetto da registrare\n" "$name" >&2
    return 1
  fi
  if ! account_exists "$name"; then
    printf "account %s creato ma non risolubile: nessun oggetto da registrare\n" "$name" >&2
    return 1
  fi
  uid=$(account_uid "$name") || uid=""
  if [[ -z $uid ]]; then
    printf "uid dell'account %s illeggibile: la cleanup non potrebbe riconoscerlo\n" "$name" >&2
    return 1
  fi
  CREATED_ACCOUNTS+=("$name:$uid")
  pass "account effimero creato e registrato per la pulizia: $name (uid $uid)"
}

uid_of() { # $1 nome -> uid registrato, o esito 1 se non creato da questa esecuzione
  local pair
  for pair in ${CREATED_ACCOUNTS[@]+"${CREATED_ACCOUNTS[@]}"}; do
    if [[ ${pair%%:*} == "$1" ]]; then
      printf '%s' "${pair##*:}"
      return 0
    fi
  done
  return 1
}

cleanup() {
  if [[ $CLEANED -eq 1 ]]; then
    return 0
  fi
  CLEANED=1
  # Solo il self-test popola questa variabile, per contare le pulizie: senza di
  # essa non viene scritto nulla e nessuna decisione operativa cambia.
  if [[ -n ${TB08_CLEANUP_LOG:-} ]]; then
    printf 'cleanup\n' >>"$TB08_CLEANUP_LOG" 2>/dev/null || true
  fi
  # Il trap EXIT viene rimosso e i segnali successivi ignorati: la pulizia
  # arriva a termine e non può esserne eseguita una seconda.
  trap - EXIT
  trap '' INT TERM HUP
  set +e
  if [[ -n $DAEMON_PID ]]; then
    if kill -0 "$DAEMON_PID" 2>/dev/null; then
      kill -TERM "$DAEMON_PID" 2>/dev/null
      local _i
      for _i in 1 2 3 4 5 6 7 8 9 10; do
        kill -0 "$DAEMON_PID" 2>/dev/null || break
        sleep 0.5
      done
      kill -KILL "$DAEMON_PID" 2>/dev/null
      wait "$DAEMON_PID" 2>/dev/null
    fi
    DAEMON_PID=""
  fi
  local pair name uid
  for pair in ${CREATED_ACCOUNTS[@]+"${CREATED_ACCOUNTS[@]}"}; do
    name=${pair%%:*}
    uid=${pair##*:}
    pkill -KILL -u "$uid" 2>/dev/null
    userdel --force --remove "$name" 2>/dev/null
  done
  # Il registro resta consultabile come evidenza dell'esecuzione: la
  # ripetizione della cleanup è impedita dal guard CLEANED, non dalla sua
  # cancellazione.
  if [[ -n $BINDIR ]]; then
    rm -rf -- "$BINDIR"
  fi
  if [[ -n $WORK ]]; then
    rm -rf -- "$WORK"
  fi
  return 0
}

# Un segnale interrompe davvero la prova: niente prosegue dopo il gestore. I
# segnali successivi vengono ignorati mentre la pulizia è in corso, l'uscita è
# una sola e il trap EXIT è rimosso da cleanup, quindi la pulizia avviene una
# volta sola anche con più segnali.
on_signal() { # $1 segnale, $2 uscita convenzionale 128+n
  trap '' INT TERM HUP
  printf 'INTERROTTO: segnale %s ricevuto; la prova si ferma e viene pulito solo ciò che è stato creato\n' "$1" >&2
  cleanup
  exit "$2"
}

install_traps() {
  trap cleanup EXIT
  trap 'on_signal INT 130' INT
  trap "on_signal TERM 143" TERM
  trap 'on_signal HUP 129' HUP
}

# Sottoprocesso delle prove sui segnali. Esegue le stesse funzioni della prova
# privilegiata, ma soltanto con i comandi simulati del self-test: si rifiuta di
# partire se useradd, userdel, pkill, id, setpriv o runuser non si risolvono
# tutti nella directory degli stub, quindi non può creare account reali nemmeno
# se invocato a mano. Le fasi indicano dove il genitore deve attendere e
# inviare il segnale:
#   prima     attesa prima di qualunque creazione;
#   parziale  primo account creato, attesa prima del secondo.
signal_trial() { # $1 fase, $2 directory stub, $3 file di avanzamento
  local phase=$1 stubs=$2 progress=$3 i tool ok=1
  for tool in useradd userdel pkill id setpriv runuser; do
    if [[ -z $stubs || ! -x $stubs/$tool ]] || [[ $(PATH="$stubs:$PATH" command -v "$tool") != "$stubs/$tool" ]]; then
      ok=0
    fi
  done
  if (( ok == 0 )); then
    printf 'signal-trial richiede gli stub dei comandi: nessun account reale viene creato\n' >&2
    return 2
  fi
  export PATH="$stubs:$PATH"
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/tb08-signal.XXXXXX")
  install_traps
  case $phase in
    prima)
      printf '%s\n' "READY $phase $$" >>"$progress"
      for i in $(seq 1 600); do sleep 0.05; done
      create_account t8sigprimo "$WORK/home" || return 1
      ;;
    parziale)
      create_account t8sigprimo "$WORK/home" || return 1
      printf '%s\n' "CREATO $phase $$" >>"$progress"
      for i in $(seq 1 600); do sleep 0.05; done
      create_account t8sigsecondo "$WORK/home2" || return 1
      ;;
    *)
      printf 'fase sconosciuta: %s\n' "$phase" >&2
      return 2
      ;;
  esac
  printf '%s\n' "DOPO $phase" >>"$progress"
}

# ---------------------------------------------------------------------------
# Verdetti: diniego dimostrato e rifiuto operativo del secondo daemon.
# ---------------------------------------------------------------------------

run_as_b() {
  if ((${#RUN_AS_PREFIX[@]} == 0)); then
    env LC_ALL=C "$@"
  else
    env LC_ALL=C "${RUN_AS_PREFIX[@]}" "$@"
  fi
}

# Codici con cui lo strumento dell'operazione segnala un accesso negato. Lo
# stesso numero ha significati diversi fra strumenti: GNU ls usa 2 quando non
# può aprire una directory o un argomento, mentre per cat, touch, ln, mv, chmod
# e la redirezione di bash il 2 è un errore d'uso e il diniego esce con 1.
# L'uscita da sola non basta: occorre anche la diagnosi del kernel, verificata
# separatamente da denial_evidenced.
denial_codes() { # $1 operazione -> codici ammessi; esito 1 se sconosciuta
  case $1 in
    list) printf '1 2' ;;
    read_config | read_identity | read_controllers | read_lock | open_lock | \
      create_file | create_link | rename | replace | chmod | swap_dir) printf '1' ;;
    *) return 1 ;;
  esac
}

# Restituisce 0 solo se l'esito di quell'operazione dimostra un accesso negato
# dal sistema: codice ammesso per lo strumento, non un timeout (124 e 125), una
# mancata esecuzione (126), un comando assente (127) o una terminazione per
# segnale (>=128), e testo dell'errore del kernel. Un'uscita non nulla non
# ammessa dallo strumento è un difetto dell'harness, non un diniego.
denial_evidenced() { # $1 uscita, $2 file di output, $3 operazione
  local code=$1 file=$2 op=$3 codes
  if ! codes=$(denial_codes "$op"); then
    return 1
  fi
  case " $codes " in
    *" $code "*) ;;
    *) return 1 ;;
  esac
  if (( code >= 128 )); then
    return 1
  fi
  if [[ ! -f $file ]]; then
    return 1
  fi
  if ! grep -q -e "Permission denied" -e "Operation not permitted" "$file"; then
    return 1
  fi
  return 0
}

# Operazione sul bersaglio, eseguita come secondo account.
deny_probe() { # $1 operazione, $2 bersaglio
  case $1 in
    list) run_as_b ls -- "$2" ;;
    read_config) run_as_b cat -- "$2/config.json" ;;
    read_identity) run_as_b cat -- "$2/identity.json" ;;
    read_controllers) run_as_b cat -- "$2/controllers.json" ;;
    read_lock) run_as_b cat -- "$2/state.lock" ;;
    open_lock) run_as_b bash -c 'exec 3>"$1/state.lock"' _ "$2" ;;
    create_file) run_as_b touch -- "$2/oggetto-esterno" ;;
    create_link) run_as_b ln -s /etc/passwd "$2/collegamento-esterno" ;;
    rename) run_as_b mv -- "$2" "$2.rinominato" ;;
    replace) run_as_b bash -c 'rm -rf -- "$1" && mkdir -- "$1"' _ "$2" ;;
    chmod) run_as_b chmod 0777 "$2" ;;
    swap_dir) run_as_b mv -- "$DECOY" "$2" ;;
    *)
      printf "operazione sconosciuta: %s\n" "$1" >&2
      return 64
      ;;
  esac
}

# Albero posseduto dal secondo account: dimostra che lo stesso strumento, sulla
# stessa forma di percorso, funziona quando il bersaglio è accessibile. Senza
# questo controllo un diniego potrebbe essere un guasto dell'harness.
prepare_own_tree() {
  rm -rf -- "$OWN_TREE" "$OWN_TREE.rinominato" "$DECOY" 2>/dev/null || true
  if ! run_as_b mkdir -p "$OWN_TREE" "$DECOY"; then
    fail "il secondo account non può preparare il proprio albero di controllo"
  fi
  if ! run_as_b bash -c 'for f in config.json identity.json controllers.json state.lock; do : > "$1/$f"; done' _ "$OWN_TREE"; then
    fail "il secondo account non può creare i propri file di controllo"
  fi
  chmod 0755 "$OWN_TREE" "$DECOY"
}

must_be_denied() { # $1 descrizione, $2 operazione
  local description=$1 op=$2 code=0
  prepare_own_tree
  if [[ $op == swap_dir ]]; then
    rm -rf -- "$OWN_TREE"
  fi
  if ! deny_probe "$op" "$OWN_TREE" >"$WORK/control.log" 2>&1; then
    sed 's/^/  /' "$WORK/control.log" >&2
    fail "controllo positivo fallito su $description: l'harness non è sano e il diniego non dimostrerebbe nulla"
  fi
  prepare_own_tree
  if deny_probe "$op" "$STATE" >"$WORK/deny.log" 2>&1; then
    sed 's/^/  /' "$WORK/deny.log" >&2
    fail "$description: il tentativo del secondo account è riuscito"
  else
    code=$?
  fi
  if ! denial_evidenced "$code" "$WORK/deny.log" "$op"; then
    sed 's/^/  /' "$WORK/deny.log" >&2
    fail "$description: uscita $code non attribuibile a un accesso negato dal sistema per $op"
  fi
  pass "$description: accesso negato dal sistema (uscita $code)"
}

# Il rifiuto deve essere l'errore operativo del daemon (uscita 1) con una
# diagnosi attribuibile a proprietà o protezioni della radice altrui, e senza
# dichiarazione di prontezza. Altri esiti sono difetti della prova.
assert_daemon_refused() { # $1 uscita, $2 stderr, $3 radice attesa nella diagnosi
  local code=$1 file=$2 root=$3
  if (( code != 1 )); then
    return 1
  fi
  if grep -q " pronto:" "$file"; then
    return 1
  fi
  if ! grep -q "avvio non riuscito" "$file"; then
    return 1
  fi
  if ! grep -qE "proprietario errato|modalità errata|protezione|Permission denied|Operation not permitted" "$file"; then
    return 1
  fi
  if ! grep -q -- "$root" "$file"; then
    return 1
  fi
  return 0
}

refusal_label() { # $1 uscita -> diagnosi dell'esito inatteso
  case $1 in
    0) say "uscita 0: il secondo daemon non ha rifiutato nulla" ;;
    1) say "uscita 1: errore operativo, il codice atteso" ;;
    2) say "uscita 2: uso scorretto degli argomenti, la prova non è conclusiva" ;;
    124) say "uscita 124: timeout, il secondo daemon non termina" ;;
    125) say "uscita 125: timeout con kill forzato, il secondo daemon non termina" ;;
    126) say "uscita 126: binario non eseguibile dal secondo account" ;;
    127) say "uscita 127: comando non disponibile al secondo account" ;;
    139) say "uscita 139: terminazione per segnale 11, crash e non rifiuto" ;;
    *) say "uscita $1: inattesa" ;;
  esac
}

manifest() {
  find "$STATE" -mindepth 1 -maxdepth 1 -printf '%f %y %m %U %G %s %T@\n' | LC_ALL=C sort
  find "$STATE" -maxdepth 1 -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum
}

# ---------------------------------------------------------------------------
# Prova privilegiata.
# ---------------------------------------------------------------------------

new_suffix() { printf '%s%04d' "$$" $((RANDOM % 10000)); }

pick_names() {
  local attempt suffix
  for attempt in 1 2 3 4 5; do
    suffix=$(new_suffix)
    USER_A="t8a$suffix"
    USER_B="t8b$suffix"
    if ! account_exists "$USER_A" && ! account_exists "$USER_B"; then
      pass "nomi effimeri univoci scelti: $USER_A, $USER_B"
      return 0
    fi
  done
  return 1
}

run_proof() {
  WORK=$(mktemp -d /tmp/totum-tb08.XXXXXX)
  BINDIR=$(mktemp -d /tmp/totum-tb08-bin.XXXXXX)
  chmod 0755 "$BINDIR"
  install -m 0755 "$BINARY" "$BINDIR/totum-daemon"
  DAEMON_BIN="$BINDIR/totum-daemon"
  install_traps

  if ! pick_names; then
    die_env "impossibile ottenere due nomi di account effimeri univoci"
  fi
  HOME_A="/home/$USER_A"
  HOME_B="/home/$USER_B"
  STATE_PARENT="$HOME_A/.local/state"
  STATE="$STATE_PARENT/totum"
  CONTROL_PARENT="$HOME_B/controls"
  OWN_TREE="$CONTROL_PARENT/totum-copy"
  DECOY="$CONTROL_PARENT/altrove"

  create_account "$USER_A" "$HOME_A" || die_env "ambiente inadeguato: primo account non ottenibile"
  UID_A=$(uid_of "$USER_A") || fail "registrazione del primo account persa"
  GID_A=$(account_gid "$USER_A") || die_env "gid del primo account illeggibile"
  create_account "$USER_B" "$HOME_B" || die_env "ambiente inadeguato: secondo account non ottenibile"
  UID_B=$(uid_of "$USER_B") || fail "registrazione del secondo account persa"
  RUN_AS_PREFIX=(runuser -u "$USER_B" --)

  # Le home restano attraversabili e i componenti intermedi sono predisposti
  # prima del primo avvio: così il diniego dipende dalla sola radice di stato.
  chmod 0755 "$HOME_A" "$HOME_B"
  install -d -m 0755 -o "$UID_A" -g "$GID_A" "$HOME_A/.local" "$STATE_PARENT"
  install -d -m 0755 -o "$UID_B" -g "$(account_gid "$USER_B")" "$CONTROL_PARENT"

  # --- Primo account: stato e daemon residente ------------------------------
  env HOME="$HOME_A" XDG_STATE_HOME="$STATE_PARENT" \
    setpriv --reuid="$UID_A" --regid="$GID_A" --clear-groups "$DAEMON_BIN" \
    >"$WORK/daemon-a.out" 2>"$WORK/daemon-a.err" &
  DAEMON_PID=$!

  local ready=0 _i name before after status
  for _i in $(seq 1 300); do
    if grep -q " pronto:" "$WORK/daemon-a.err"; then
      ready=1
      break
    fi
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
      break
    fi
    sleep 0.1
  done
  if [[ $ready -ne 1 ]]; then
    cat "$WORK/daemon-a.out" >&2
    cat "$WORK/daemon-a.err" >&2
    fail "il daemon del primo account non è diventato pronto"
  fi
  pass "il daemon del primo account è pronto e residente"

  if [[ ! -d $STATE ]]; then
    fail "la radice di stato non esiste: $STATE"
  fi
  for name in config.json identity.json controllers.json state.lock; do
    if [[ ! -e $STATE/$name ]]; then
      fail "oggetto atteso assente: $name"
    fi
  done
  if [[ $(stat -c '%a %u' "$STATE") != "700 $UID_A" ]]; then
    fail "radice di stato inattesa: $(stat -c '%a %u' "$STATE")"
  fi
  if [[ $(stat -c '%a %u' "$STATE/state.lock") != "600 $UID_A" ]]; then
    fail "state.lock inatteso: $(stat -c '%a %u' "$STATE/state.lock")"
  fi
  pass "radice 0700 e state.lock 0600 del primo account"

  before=$(manifest)

  # L'attraversamento di home e antenati deve essere possibile: senza questa
  # condizione il diniego non sarebbe attribuibile alla sola radice di stato.
  if ! run_as_b test -x "$HOME_A"; then
    fail "la home del primo account non è attraversabile dal secondo: la prova non sarebbe conclusiva"
  fi
  if ! run_as_b test -x "$STATE_PARENT"; then
    fail "gli antenati della radice non sono attraversabili dal secondo: la prova non sarebbe conclusiva"
  fi
  pass "home e antenati attraversabili dal secondo account"

  # --- Secondo account: ogni tentativo deve essere negato -------------------
  must_be_denied "elenco della radice di stato" list
  must_be_denied "lettura di config.json" read_config
  must_be_denied "lettura di identity.json" read_identity
  must_be_denied "lettura di controllers.json" read_controllers
  must_be_denied "lettura di state.lock" read_lock
  must_be_denied "apertura in scrittura di state.lock" open_lock
  must_be_denied "creazione di un file nella radice" create_file
  must_be_denied "creazione di un collegamento nella radice" create_link
  must_be_denied "rinomina della radice di stato" rename
  must_be_denied "svuotamento e ricreazione della radice" replace
  must_be_denied "modifica delle protezioni della radice" chmod
  must_be_denied "sostituzione della radice con un'altra directory" swap_dir

  # --- Secondo daemon ------------------------------------------------------
  set +e
  timeout 15 runuser -u "$USER_B" -- env HOME="$HOME_A" XDG_STATE_HOME="$STATE_PARENT" \
    "$DAEMON_BIN" >"$WORK/daemon-b.out" 2>"$WORK/daemon-b.err"
  status=$?
  set -e
  if ! assert_daemon_refused "$status" "$WORK/daemon-b.err" "$STATE"; then
    sed 's/^/  /' "$WORK/daemon-b.err" >&2
    refusal_label "$status"
    fail "il secondo daemon non ha prodotto il rifiuto operativo atteso (uscita $status)"
  fi
  pass "secondo daemon: uscita 1 con diagnosi di proprietà o protezioni, nessuna prontezza"

  # --- Integrità e terminazione -------------------------------------------
  if [[ $(stat -c '%a %u' "$STATE") != "700 $UID_A" ]]; then
    fail "le protezioni della radice sono cambiate durante i tentativi"
  fi
  after=$(manifest)
  if [[ $before != "$after" ]]; then
    say "prima:" >&2
    printf '%s\n' "$before" | sed 's/^/  /' >&2
    say "dopo:" >&2
    printf '%s\n' "$after" | sed 's/^/  /' >&2
    fail "lo stato del primo account è cambiato dopo i tentativi"
  fi
  pass "integrità dello stato invariata dopo i tentativi"

  if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
    fail "il daemon del primo account è terminato durante le prove"
  fi
  kill -TERM "$DAEMON_PID"
  local stopped=0
  for _i in $(seq 1 100); do
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
      stopped=1
      break
    fi
    sleep 0.1
  done
  if [[ $stopped -ne 1 ]]; then
    fail "il daemon del primo account non termina con SIGTERM"
  fi
  wait "$DAEMON_PID" 2>/dev/null || true
  DAEMON_PID=""
  pass "il daemon del primo account termina con SIGTERM"

  say "TB-08 superato: account creati e rimossi da questa esecuzione: ${CREATED_ACCOUNTS[*]}"
}

# ---------------------------------------------------------------------------
# Self-test non privilegiato: comandi simulati, nessun account reale.
# ---------------------------------------------------------------------------

make_stub_dir() { # $1 directory, $2 valore di uname -s
  local dir=$1 osname=$2 tool
  install -d "$dir"
  cat >"$dir/uname" <<EOF
#!/usr/bin/env bash
printf '$osname\\n'
EOF
  cat >"$dir/id" <<'EOF'
#!/usr/bin/env bash
name=""
flag=""
for a in "$@"; do
  case $a in
  --) ;;
  -*) flag=$a ;;
  *) name=$a ;;
  esac
done
printf 'id %s\n' "$*" >>"${TB08_STUB_LOG:-/dev/null}"
if [[ -z $name ]]; then
  exit 0
fi
if grep -q "^$name|" "${TB08_STUB_DB:-/dev/null}" 2>/dev/null; then
  case $flag in
  -u) printf '%s\n' "$(grep "^$name|" "$TB08_STUB_DB" | cut -d'|' -f2)" ;;
  -g) printf '%s\n' "$(grep "^$name|" "$TB08_STUB_DB" | cut -d'|' -f3)" ;;
  esac
  exit 0
fi
exit 1
EOF
  cat >"$dir/useradd" <<'EOF'
#!/usr/bin/env bash
name="${@: -1}"
printf 'useradd %s\n' "$name" >>"${TB08_STUB_LOG:-/dev/null}"
printf 'useradd %s\n' "$name" >>"${TB08_STUB_TOTAL:-/dev/null}"
if [[ -n ${TB08_FAIL_USERADD:-} && $name == "$TB08_FAIL_USERADD" ]]; then
  printf 'useradd: fallimento simulato per %s\n' "$name" >&2
  exit 9
fi
seq=$(( $(grep -c '' "${TB08_STUB_DB:-/dev/null}" 2>/dev/null || true) + 1 ))
printf '%s|%s|%s\n' "$name" "$((41233 + seq))" "$((41233 + seq))" >>"$TB08_STUB_DB"
exit 0
EOF
  cat >"$dir/userdel" <<'EOF'
#!/usr/bin/env bash
name="${@: -1}"
printf 'userdel %s\n' "$name" >>"${TB08_STUB_LOG:-/dev/null}"
printf 'userdel %s\n' "$name" >>"${TB08_STUB_TOTAL:-/dev/null}"
if [[ -n ${TB08_STUB_DB:-} && -f $TB08_STUB_DB ]]; then
  grep -v "^$name|" "$TB08_STUB_DB" >"$TB08_STUB_DB.new" 2>/dev/null || true
  mv "$TB08_STUB_DB.new" "$TB08_STUB_DB"
fi
exit 0
EOF
  for tool in pkill setpriv runuser; do
    cat >"$dir/$tool" <<EOF
#!/usr/bin/env bash
printf '$tool %s\\n' "\$*" >>"\${TB08_STUB_LOG:-/dev/null}"
exit 0
EOF
  done
  chmod 0755 "$dir"/*
}

seed_account() { printf '%s|41999|41999\n' "$1" >>"$DB"; }

new_box() {
  local box
  box=$(mktemp -d "$WORK/box.XXXXXX")
  BINDIR=$box/bin
  install -d "$BINDIR"
  printf '%s' "$box"
}

count_log() { # $1 modello -> numero di righe corrispondenti nel registro stub
  local n
  n=$(grep -c "$1" "$LOG" 2>/dev/null || true)
  printf '%s' "${n:-0}"
}

mutation_lines() {
  grep -e '^useradd ' -e '^userdel ' -e '^pkill ' -e '^setpriv ' -e '^runuser ' "$LOG" 2>/dev/null || true
}

count_mutations() {
  local n
  n=$(mutation_lines | grep -c '' 2>/dev/null || true)
  printf '%s' "${n:-0}"
}

count_file_lines() { # $1 file -> numero di righe
  local n
  n=$(grep -c '' "$1" 2>/dev/null || true)
  printf '%s' "${n:-0}"
}

assert_no_mutation() {
  local hits
  hits=$(count_mutations)
  if [[ $hits != 0 ]]; then
    mutation_lines >&2
    fail "il rifiuto ambientale ha comunque invocato strumenti di modifica ($hits)"
  fi
}

assert_stub_isolation() {
  local tool found
  for tool in useradd userdel id pkill setpriv runuser uname; do
    found=$(PATH="$STUBS:$PATH" command -v "$tool" || true)
    if [[ $found != "$STUBS/$tool" ]]; then
      fail "$tool non si risolve nello stub: $found"
    fi
  done
}

run_scenario() { # $1 nome della funzione di scenario
  local status=0
  say "-- scenario: $1"
  ( "$1" ) || status=$?
  if (( status != 0 )); then
    fail "scenario $1 non superato (uscita $status)"
  fi
}

invoke_guards() { # $1 directory stub -> scrive esito e uscita in variabili globali
  GUARD_OUT="$WORK/guard.txt"
  GUARD_STATUS=0
  PATH="$1:$PATH" TB08_SELFTEST_PROBE=1 TOTUM_TB08_CI="$WANT_CI" CI="$WANT_CI_FLAG" \
    GITHUB_ACTIONS="$WANT_ACTIONS" RUNNER_ENVIRONMENT="$WANT_RUNNER" \
    bash "$SCRIPT_PATH" "$SCRIPT_PATH" >"$GUARD_OUT" 2>&1 || GUARD_STATUS=$?
}

scenario_non_linux() {
  WANT_CI=1
  WANT_CI_FLAG=true
  WANT_ACTIONS=true
  WANT_RUNNER=github-hosted
  : >"$LOG"
  invoke_guards "$STUBS_DARWIN"
  if (( GUARD_STATUS != 2 )); then
    fail "piattaforma non Linux: uscita $GUARD_STATUS, atteso 2"
  fi
  if ! grep -q "richiede Linux" "$GUARD_OUT"; then
    fail "piattaforma non Linux: diagnosi sul vincolo Linux assente"
  fi
  assert_no_mutation
  pass "esecuzione fuori da Linux: rifiutata prima di ogni modifica"
}

scenario_missing_ci_confirmation() {
  WANT_CI=0
  WANT_CI_FLAG=
  WANT_ACTIONS=
  WANT_RUNNER=github-hosted
  : >"$LOG"
  invoke_guards "$STUBS"
  if (( GUARD_STATUS != 2 )); then
    fail "senza conferma CI: uscita $GUARD_STATUS, atteso 2"
  fi
  if ! grep -q "TOTUM_TB08_CI" "$GUARD_OUT"; then
    fail "senza conferma CI: diagnosi assente"
  fi
  if grep -q "richiede root" "$GUARD_OUT"; then
    fail "il vincolo CI deve precedere quello sui privilegi: root non può essere la sola condizione"
  fi
  assert_no_mutation
  pass "esecuzione senza TOTUM_TB08_CI=1: rifiutata prima del controllo sui privilegi"
}

scenario_missing_ci_context() {
  WANT_CI=1
  WANT_CI_FLAG=
  WANT_ACTIONS=
  WANT_RUNNER=github-hosted
  : >"$LOG"
  invoke_guards "$STUBS"
  if (( GUARD_STATUS != 2 )); then
    fail "senza contesto CI: uscita $GUARD_STATUS, atteso 2"
  fi
  if ! grep -q "attraversare sudo" "$GUARD_OUT"; then
    fail "senza contesto CI: diagnosi assente"
  fi
  assert_no_mutation
  pass "contesto CI non trasmesso attraverso sudo: rifiutato"
}

scenario_self_hosted_runner() {
  WANT_CI=1
  WANT_CI_FLAG=true
  WANT_ACTIONS=true
  WANT_RUNNER=
  : >"$LOG"
  invoke_guards "$STUBS"
  if (( GUARD_STATUS != 2 )); then
    fail "runner self-hosted: uscita $GUARD_STATUS, atteso 2"
  fi
  if ! grep -q "GitHub-hosted" "$GUARD_OUT"; then
    fail "runner self-hosted: diagnosi assente"
  fi
  assert_no_mutation
  pass "runner non dichiarato GitHub-hosted: rifiutato"
}

scenario_root_not_enough() {
  WANT_CI=1
  WANT_CI_FLAG=true
  WANT_ACTIONS=true
  WANT_RUNNER=github-hosted
  : >"$LOG"
  invoke_guards "$STUBS"
  if [[ $EUID -eq 0 ]]; then
    if (( GUARD_STATUS != 0 )) || ! grep -q "sonda" "$GUARD_OUT"; then
      fail "sonda privilegiata: uscita $GUARD_STATUS senza messaggio di arresto"
    fi
  else
    if (( GUARD_STATUS != 2 )); then
      fail "senza root: uscita $GUARD_STATUS, atteso 2"
    fi
    if ! grep -q "richiede root" "$GUARD_OUT"; then
      fail "senza root: diagnosi assente"
    fi
  fi
  assert_no_mutation
  pass "privilegio e contesto CI verificati separatamente, senza alcuna modifica"
}

scenario_initial_collision() {
  local box out
  box=$(new_box)
  WORK=$box
  out=$box/out.txt
  PATH="$STUBS:$PATH"
  : >"$LOG"
  : >"$DB"
  seed_account tb08preesistente
  if create_account tb08preesistente "$box/home" >"$out" 2>&1; then
    cat "$out" >&2
    fail "collisione iniziale non rilevata: l'account preesistente è stato accettato"
  fi
  if [[ ${#CREATED_ACCOUNTS[@]} -ne 0 ]]; then
    fail "un account preesistente è stato registrato fra quelli creati"
  fi
  cleanup
  if [[ $(count_log '^userdel tb08preesistente$') != 0 ]]; then
    fail "userdel invocato su un account preesistente"
  fi
  if [[ $(count_log '^useradd tb08preesistente$') != 0 ]]; then
    fail "useradd invocato nonostante la collisione"
  fi
  if [[ $(count_log '^pkill ') != 0 ]]; then
    fail "pkill invocato per un account non creato da questa esecuzione"
  fi
  pass "collisione iniziale: account preesistente intatto ed escluso dalla cleanup"
}

scenario_partial_creation() {
  local box
  box=$(new_box)
  WORK=$box
  PATH="$STUBS:$PATH"
  export TB08_FAIL_USERADD=tb08secondo
  : >"$LOG"
  : >"$DB"
  if ! create_account tb08primo "$box/h1"; then
    unset TB08_FAIL_USERADD
    fail "il primo account avrebbe dovuto essere creato"
  fi
  if create_account tb08secondo "$box/h2" 2>"$box/useradd.err"; then
    unset TB08_FAIL_USERADD
    fail "la creazione simulata del secondo account avrebbe dovuto fallire"
  fi
  if [[ ${#CREATED_ACCOUNTS[@]} -ne 1 ]]; then
    unset TB08_FAIL_USERADD
    fail "registro degli account creati: attesa 1 voce, trovate ${#CREATED_ACCOUNTS[@]}"
  fi
  cleanup
  unset TB08_FAIL_USERADD
  if [[ $(count_log '^userdel tb08primo$') != 1 ]]; then
    fail "il primo account creato non viene rimosso una sola volta"
  fi
  if [[ $(count_log '^userdel tb08secondo$') != 0 ]]; then
    fail "userdel invocato per un account mai creato"
  fi
  if [[ $(count_log '^pkill ') != 1 ]]; then
    fail "pkill invocato un numero di volte diverso da 1 per il solo account creato"
  fi
  if ! grep -q '^pkill -KILL -u 41234$' "$LOG"; then
    fail "la cleanup non usa l'uid registrato al momento della creazione"
  fi
  pass "creazione parziale: rimossa solo la parte creata, riconoscendola dall'uid"
}

scenario_repeated_cleanup() {
  local box
  box=$(new_box)
  WORK=$box
  PATH="$STUBS:$PATH"
  : >"$LOG"
  : >"$DB"
  if ! create_account tb08solo "$box/h"; then
    fail "l'account simulato non è stato creato"
  fi
  cleanup
  cleanup
  cleanup
  if [[ $(count_log '^userdel tb08solo$') != 1 ]]; then
    fail "cleanup ripetuta ha invocato userdel più di una volta"
  fi
  if [[ $(count_log '^pkill ') != 1 ]]; then
    fail "cleanup ripetuta ha invocato pkill più di una volta"
  fi
  pass "cleanup ripetuta: idempotente, una sola rimozione per account creato"
}

# Avvia un sottoprocesso della prova sui segnali, attende il punto concordato,
# invia uno o due segnali e ne raccoglie l'esito. Il figlio gira in primo piano
# — un comando avviato in background erediterebbe SIGINT ignorato e non
# potrebbe installarne il gestore — mentre un osservatore in background legge
# il pid annunciato dal figlio e invia i segnali. Il sottoprocesso usa soltanto
# gli stub: nessun account reale viene creato.
run_signal_case() { # $1 descrizione, $2 fase, $3 segnale, $4 attesa, [$5 secondo segnale]
  local description=$1 phase=$2 sig=$3 wait_text=$4 second=${5:-} box child_pid=
  box=$(mktemp -d "$WORK/case.XXXXXX")
  SIGNAL_CASE=$box
  SIGNAL_PROGRESS=$box/progress.txt
  : >"$SIGNAL_PROGRESS"
  : >"$box/cleanup.log"
  (
    local i line
    for i in $(seq 1 300); do
      line=$(grep -- "$wait_text" "$SIGNAL_PROGRESS" 2>/dev/null | cut -d' ' -f3 | head -n 1 || true)
      if [[ -n $line ]]; then
        child_pid=$line
        break
      fi
      sleep 0.05
    done
    if [[ -z $child_pid ]]; then
      exit 1
    fi
    kill -s "$sig" "$child_pid" 2>/dev/null || exit 1
    if [[ -n $second ]]; then
      kill -s "$second" "$child_pid" 2>/dev/null || true
    fi
  ) &
  local watcher=$!
  set +e
  PATH="$STUBS:$PATH" TB08_CLEANUP_LOG="$box/cleanup.log" \
    bash "$SCRIPT_PATH" --signal-trial "$phase" "$STUBS" "$SIGNAL_PROGRESS" \
    >"$box/child.out" 2>"$box/child.err"
  SIGNAL_STATUS=$?
  set -e
  wait "$watcher" 2>/dev/null || true
  if ! grep -q -- "$wait_text" "$SIGNAL_PROGRESS" 2>/dev/null; then
    cat "$box/child.err" >&2
    fail "$description: il sottoprocesso non ha raggiunto il punto di attesa"
  fi
  SIGNAL_CLEANUPS=$(count_file_lines "$box/cleanup.log")
}

scenario_signal_before_creation() {
  local pair sig expected description
  for pair in "INT 130" "TERM 143" "HUP 129"; do
    sig=${pair%% *}
    expected=${pair##* }
    description="segnale $sig prima di qualunque creazione"
    : >"$LOG"
    : >"$DB"
    run_signal_case "$description" prima "$sig" "READY prima"
    if (( SIGNAL_STATUS != expected )); then
      cat "$SIGNAL_CASE/child.err" >&2
      fail "$description: uscita $SIGNAL_STATUS, attesa $expected"
    fi
    if ! grep -q "INTERROTTO: segnale $sig" "$SIGNAL_CASE/child.err"; then
      fail "$description: diagnosi di interruzione assente"
    fi
    if [[ $(count_log '^useradd ') != 0 ]]; then
      mutation_lines >&2
      fail "$description: un'operazione di creazione è avvenuta dopo il segnale"
    fi
    if grep -q 'DOPO prima' "$SIGNAL_PROGRESS"; then
      fail "$description: lo script è proseguito dopo il segnale"
    fi
    if (( SIGNAL_CLEANUPS != 1 )); then
      fail "$description: pulizie eseguite $SIGNAL_CLEANUPS, attesa 1"
    fi
  done
  pass "INT, TERM e HUP prima della creazione: uscita 128+n, nessuna operazione successiva, una sola pulizia"
}

scenario_signal_after_partial_creation() {
  : >"$LOG"
  : >"$DB"
  run_signal_case "segnale TERM dopo una creazione parziale" parziale TERM "CREATO parziale"
  if (( SIGNAL_STATUS != 143 )); then
    cat "$SIGNAL_CASE/child.err" >&2
    fail "creazione parziale interrotta: uscita $SIGNAL_STATUS, attesa 143"
  fi
  if [[ $(count_log '^userdel t8sigprimo$') != 1 ]]; then
    mutation_lines >&2
    fail "l'account registrato non è stato rimosso esattamente una volta"
  fi
  if [[ $(count_log '^userdel t8sigsecondo$') != 0 ]]; then
    fail "è stato rimosso un account mai creato"
  fi
  if [[ $(count_log '^useradd ') != 1 ]]; then
    mutation_lines >&2
    fail "il secondo account è stato creato dopo il segnale"
  fi
  if [[ $(count_log '^pkill ') != 1 ]]; then
    fail "pkill invocato $(count_log '^pkill ') volte invece di una"
  fi
  if grep -q 'DOPO parziale' "$SIGNAL_PROGRESS"; then
    fail "lo script è proseguito dopo il segnale"
  fi
  if (( SIGNAL_CLEANUPS != 1 )); then
    fail "pulizie eseguite $SIGNAL_CLEANUPS, attesa 1"
  fi
  if grep -q '^t8sigprimo|' "$DB"; then
    fail "il registro simulato contiene ancora l'account rimosso"
  fi

  # Un secondo segnale non provoca una seconda pulizia né una seconda rimozione.
  : >"$LOG"
  : >"$DB"
  run_signal_case "segnale TERM seguito da INT" parziale TERM "CREATO parziale" INT
  if (( SIGNAL_STATUS < 129 )); then
    cat "$SIGNAL_CASE/child.err" >&2
    fail "doppio segnale: uscita $SIGNAL_STATUS, attesa una terminazione da segnale"
  fi
  if (( SIGNAL_CLEANUPS != 1 )); then
    fail "doppio segnale: pulizie eseguite $SIGNAL_CLEANUPS, attesa 1"
  fi
  if [[ $(count_log '^userdel t8sigprimo$') != 1 ]]; then
    mutation_lines >&2
    fail "doppio segnale: rimozione doppia o assente dell'account registrato"
  fi
  if [[ $(count_log '^useradd ') != 1 ]]; then
    fail "doppio segnale: creazione avvenuta dopo l'interruzione"
  fi
  pass "creazione parziale interrotta: solo l'account registrato rimosso una volta, nessuna operazione dopo il segnale, anche con due segnali"
}

expect_denial() { # $1 operazione, $2 uscita, $3 testo, $4 atteso (si|no)
  local file="$WORK/denial-case.log" verdict=no
  printf '%s\n' "$3" >"$file"
  if denial_evidenced "$2" "$file" "$1"; then
    verdict=si
  fi
  if [[ $verdict != "$4" ]]; then
    fail "diniego $1 con uscita $2 e testo «$3»: classificato $verdict, atteso $4"
  fi
}

scenario_denial_classification() {
  # GNU ls segnala con 2 l'argomento che non riesce ad aprire: con una radice
  # 0000 il diniego esce 2, non 1. Vale solo per ls e solo insieme alla
  # diagnosi del kernel.
  expect_denial list 2 "ls: cannot open directory 'x': Permission denied" si
  expect_denial list 1 "ls: cannot access 'x': Permission denied" si
  expect_denial list 1 "ls: x: Operation not permitted" si
  # Percorso inesistente, argomento errato e guasti non sono dinieghi, anche
  # quando ls usa lo stesso codice 2.
  expect_denial list 2 "ls: cannot access 'x': No such file or directory" no
  expect_denial list 2 "ls: unrecognized option '--inesistente'" no
  expect_denial list 1 "ls: cannot access 'x': No such file or directory" no
  expect_denial list 0 "ls: cannot access 'x': Permission denied" no
  expect_denial list 124 "timeout: Permission denied" no
  expect_denial list 125 "timeout: Permission denied" no
  expect_denial list 126 "bash: x: Permission denied" no
  expect_denial list 127 "runuser: Permission denied" no
  expect_denial list 137 "Killed" no
  expect_denial list 139 "Segmentation fault (core dumped)" no
  # Per cat, touch, ln, mv, chmod, bash e la sostituzione della directory il 2
  # è un errore d'uso: lo stesso testo con 2 non è un diniego.
  expect_denial read_config 1 "cat: x/config.json: Permission denied" si
  expect_denial read_config 2 "cat: x/config.json: Permission denied" no
  expect_denial read_identity 1 "cat: x/identity.json: Permission denied" si
  expect_denial read_controllers 2 "cat: x/controllers.json: Permission denied" no
  expect_denial read_lock 1 "cat: x/state.lock: Operation not permitted" si
  expect_denial open_lock 1 "bash: x/state.lock: Permission denied" si
  expect_denial open_lock 2 "bash: x/state.lock: Permission denied" no
  expect_denial create_file 1 "touch: cannot touch 'x': Permission denied" si
  expect_denial create_file 2 "touch: cannot touch 'x': Permission denied" no
  expect_denial create_file 1 "touch: missing file operand" no
  expect_denial create_link 1 "ln: failed to create symbolic link 'x': Permission denied" si
  expect_denial create_link 2 "ln: failed to create symbolic link 'x': Permission denied" no
  expect_denial rename 1 "mv: cannot move 'x': Permission denied" si
  expect_denial rename 2 "mv: cannot move 'x': Permission denied" no
  expect_denial replace 1 "bash: x: Permission denied" si
  expect_denial replace 2 "bash: x: Permission denied" no
  expect_denial chmod 1 "chmod: changing permissions of 'x': Operation not permitted" si
  expect_denial chmod 2 "chmod: changing permissions of 'x': Operation not permitted" no
  expect_denial swap_dir 1 "mv: cannot move 'x': Permission denied" si
  expect_denial swap_dir 2 "mv: cannot move 'x': Permission denied" no
  # Un'operazione sconosciuta non ha un codice ammesso: nessuna uscita vale.
  expect_denial frobnicate 1 "frobnicate: Permission denied" no
  pass "classificazione dei dinieghi per strumento: codice, diagnosi, argomenti errati, percorso assente, timeout, crash e operazioni sconosciute"
}

expect_refusal() { # $1 uscita, $2 testo, $3 atteso (si|no), $4 radice
  local file="$WORK/daemon-case.log" verdict=no
  printf '%s\n' "$2" >"$file"
  if assert_daemon_refused "$1" "$file" "$4"; then
    verdict=si
  fi
  if [[ $verdict != "$3" ]]; then
    fail "secondo daemon con uscita $1: verdetto $verdict, atteso $3"
  fi
}

scenario_daemon_verdicts() {
  local root="/home/tb08sim/.local/state/totum" a b c d e
  expect_refusal 1 \
    "totum-daemon: avvio non riuscito: preparazione della radice $root fallita: proprietario errato per /home/tb08sim, atteso 1001, trovato 1000" \
    si "$root"
  expect_refusal 1 \
    "totum-daemon: avvio non riuscito: preparazione della radice $root fallita: errore di sistema su $root: Permission denied" \
    si "$root"
  expect_refusal 0 \
    "totum-daemon 0.0.0 pronto: radice di stato $root; nessun listener di rete" no "$root"
  expect_refusal 1 \
    "totum-daemon: avvio non riuscito: proprietario errato per $root; totum-daemon 0.0.0 pronto: radice di stato $root" no "$root"
  expect_refusal 2 "totum-daemon: argomento non supportato: --stato" no "$root"
  expect_refusal 127 "bash: totum-daemon: command not found" no "$root"
  expect_refusal 139 "Segmentation fault (core dumped)" no "$root"
  expect_refusal 124 "" no "$root"
  expect_refusal 1 "totum-daemon: avvio non riuscito: variabile d'ambiente HOME assente" no "$root"
  expect_refusal 1 "totum-daemon: avvio non riuscito: config.json corrotto in $root" no "$root"
  # Un arresto anomalo dopo il messaggio diagnostico non è un rifiuto: solo
  # l'uscita 1 lo è.
  expect_refusal 139 \
    "totum-daemon: avvio non riuscito: preparazione della radice $root fallita: proprietario errato per /home/tb08sim" no "$root"
  expect_refusal 2 \
    "totum-daemon: avvio non riuscito: proprietario errato per $root, poi uso scorretto degli argomenti" no "$root"
  a=$(refusal_label 0)
  b=$(refusal_label 2)
  c=$(refusal_label 124)
  d=$(refusal_label 127)
  e=$(refusal_label 139)
  if [[ -z $b || $b == "$c" || $c == "$d" || $d == "$e" || $e == "$a" ]]; then
    fail "le uscite inattese non hanno diagnosi distinte"
  fi
  pass "verdetto sul secondo daemon: solo uscita 1 con diagnosi di proprietà o protezioni"
}

prepare_sim_trees() { # $1 base
  CONTROL_PARENT=$1/control
  OWN_TREE=$CONTROL_PARENT/totum-copy
  DECOY=$CONTROL_PARENT/altrove
  WORK=$1
  RUN_AS_PREFIX=()
  install -d "$CONTROL_PARENT" "$1/stato"
}

scenario_denied_end_to_end() {
  local box f
  box=$(new_box)
  prepare_sim_trees "$box"
  STATE=$box/stato/estera
  PATH="$STUBS:$PATH"
  : >"$LOG"
  install -d "$STATE"
  for f in config.json identity.json controllers.json state.lock; do
    : >"$STATE/$f"
  done
  chmod 0000 "$STATE"
  must_be_denied "elenco della radice senza permessi" list
  must_be_denied "lettura di config.json senza permessi" read_config
  must_be_denied "lettura di state.lock senza permessi" read_lock
  must_be_denied "apertura in scrittura di state.lock senza permessi" open_lock
  must_be_denied "creazione di un file senza permessi" create_file
  must_be_denied "creazione di un collegamento senza permessi" create_link
  must_be_denied "svuotamento di una radice senza permessi" replace
  chmod 0755 "$STATE"
  if (must_be_denied "bersaglio ora accessibile" list) 2>/dev/null; then
    fail "un bersaglio accessibile è stato classificato come accesso negato"
  fi
  pass "diniego provato su alberi reali, con controlli positivi sul stesso strumento"
}

scenario_harness_is_honest() {
  local box f
  box=$(new_box)
  prepare_sim_trees "$box"
  STATE=$box/stato/totum
  PATH="$STUBS:$PATH"
  : >"$LOG"
  install -d "$STATE"
  for f in config.json identity.json controllers.json state.lock; do
    : >"$STATE/$f"
  done
  if (must_be_denied "bersaglio leggibile" list) 2>/dev/null; then
    fail "l'harness accetta come diniego un bersaglio che il secondo account può leggere"
  fi
  rm -rf -- "$STATE"
  if (must_be_denied "bersaglio inesistente" list) 2>/dev/null; then
    fail "l'harness accetta come diniego un percorso inesistente"
  fi
  pass "falsi positivi rifiutati: bersaglio leggibile e percorso inesistente"
}

scenario_no_real_accounts() {
  local found name real
  found=$(PATH=/usr/bin:/bin command -v id || true)
  if [[ -z $found ]]; then
    fail "id di sistema non reperibile: la verifica finale non sarebbe significativa"
  fi
  for name in tb08preesistente tb08primo tb08secondo tb08solo; do
    if "$found" -- "$name" >/dev/null 2>&1; then
      fail "l'account reale $name esiste: il self-test ha modificato il sistema"
    fi
  done
  real=$(id -nu 2>/dev/null || printf '%s' "${USER:-}")
  if [[ -n $real ]] && ! "$found" -u -- "$real" >/dev/null 2>&1; then
    fail "l'account reale corrente $real non è più risolubile"
  fi
  pass "nessun account reale creato o rimosso: i nomi simulati non esistono nel sistema"
}

selftest_exit() {
  if ((BASH_SUBSHELL > 0)); then
    return 0
  fi
  trap - EXIT
  if [[ ${SELFTEST_CLEANED:-0} -eq 1 ]]; then
    return 0
  fi
  SELFTEST_CLEANED=1
  if [[ -n $WORK ]]; then
    rm -rf -- "$WORK"
  fi
}

selftest_signal() { # $1 segnale, $2 uscita convenzionale 128+n
  trap '' INT TERM HUP
  printf 'INTERROTTO: segnale %s ricevuto; il self-test si ferma\n' "$1" >&2
  selftest_exit
  exit "$2"
}

self_test() {
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/tb08-selftest.XXXXXX")
  STUBS="$WORK/stubs"
  STUBS_DARWIN="$WORK/stubs-darwin"
  DB="$WORK/accounts.db"
  LOG="$WORK/stub.log"
  TOTAL="$WORK/stub-total.log"
  trap selftest_exit EXIT
  trap 'selftest_signal INT 130' INT
  trap 'selftest_signal TERM 143' TERM
  trap 'selftest_signal HUP 129' HUP
  make_stub_dir "$STUBS" Linux
  make_stub_dir "$STUBS_DARWIN" Darwin
  : >"$DB"
  : >"$LOG"
  : >"$TOTAL"
  export TB08_STUB_LOG="$LOG" TB08_STUB_DB="$DB" TB08_STUB_TOTAL="$TOTAL"
  say "self-test TB-08 con bash $BASH_VERSION: nessun account reale verrà creato"
  assert_stub_isolation
  run_scenario scenario_non_linux
  run_scenario scenario_missing_ci_confirmation
  run_scenario scenario_missing_ci_context
  run_scenario scenario_self_hosted_runner
  run_scenario scenario_root_not_enough
  run_scenario scenario_initial_collision
  run_scenario scenario_partial_creation
  run_scenario scenario_repeated_cleanup
  run_scenario scenario_signal_before_creation
  run_scenario scenario_signal_after_partial_creation
  run_scenario scenario_denial_classification
  run_scenario scenario_daemon_verdicts
  run_scenario scenario_denied_end_to_end
  run_scenario scenario_harness_is_honest
  run_scenario scenario_no_real_accounts
  say "self-test TB-08 superato: $(count_file_lines "$TOTAL") invocazioni simulate di useradd/userdel, tutte negli stub, zero account reali"
}

main() {
  if [[ ${1:-} == --self-test ]]; then
    self_test
    return 0
  fi
  if [[ ${1:-} == --signal-trial ]]; then
    shift
    signal_trial "$@"
    return 0
  fi
  if [[ $# -ne 1 ]]; then
    usage
    die_env "argomento richiesto: il percorso del binario totum-daemon"
  fi
  BINARY=$1
  require_linux
  require_ci_context
  require_root
  if [[ -n $PROBE ]]; then
    say "controlli d'ambiente superati: sonda ferma prima di ogni modifica"
    return 0
  fi
  require_tools
  if [[ ! -x $BINARY ]]; then
    die_env "il binario indicato non esiste o non è eseguibile: $BINARY"
  fi
  run_proof
}

main "$@"
