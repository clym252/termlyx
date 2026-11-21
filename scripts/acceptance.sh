#!/usr/bin/env bash
# Termlyx acceptance gate (automated part). Manual release checks live in
# RELEASING.md.
#
# Usage: scripts/acceptance.sh [step ...]   (from any directory)
# Steps, in order (default: all):
#   fixture     tests/fixtures/up.sh (Docker SSH fixture on 127.0.0.1:2222)
#   cargo       TETHER_SSH_FIXTURE=1 cargo test --workspace -- --include-ignored
#               (debug; includes the 1 GiB SFTP checksum; see the note on
#               shell_cat_throughput below)
#   throughput  the headless shell_cat_throughput benchmark, in release
#   vitest      pnpm -C apps/tauri vitest run
#   playwright  pnpm -C apps/tauri playwright test
#   server      Phase 2: builds and starts the real tether-server image
#               (docker compose project tether-accept, https://localhost:18443,
#               removed again on exit), then runs the ignored test
#               two_vaults_sync_through_real_server against it
#   sync        Phase 2: the multi-client convergence simulation, 200 seeds
#   canary      creates a host labelled CANARY-7f3a (plus an identity password,
#               a password saved as from a connect prompt, and generated keys) on a real on-disk vault through the app's
#               own AppState command functions, turns sync on against the
#               tether-accept container (started here if the server step did
#               not), then greps the vault file, WAL, shm and the run's log,
#               and the server's database, WAL, a backup and its logs, for the
#               canary and the secrets
#
# Optional steps, only when named (never in the default run; need the Android
# toolchain, so they run on devvm: scripts/android.sh build, or devvm.sh run):
#   android-clippy  cargo clippy -p tether-app -D warnings for x86_64 and aarch64
#                   Android (API 30), with the NDK's clang as C compiler and linker
#   android-apk     pnpm tauri android build: debug x86_64 APK, or the arm64-v8a
#                   release APK when TETHER_ANDROID_RELEASE=1 (unsigned); prints its path.
#                   TETHER_ANDROID_TARGET=aarch64 makes the debug APK for a real device.
#                   TETHER_ANDROID_AAB=1: the arm64-v8a release AAB for Play (unsigned) with
#                   versionCode TETHER_ANDROID_VERSION_CODE; prints `AAB: <path>`
#
#   deps            cargo deny check (licenses advisories bans sources), npm audit
#                   (prod, high) and the npm license check; needs cargo-deny
#
# On the Mac, Docker (fixture, cargo, throughput, server, canary) runs on devvm by default:
# scripts/devvm.sh opens an SSH tunnel (localhost:2222, localhost:18443 and
# devvm's Docker socket) and DOCKER_HOST points the docker CLI at it; Docker
# Desktop need not run. A tunnel this run opened is closed on exit; the
# containers stay on devvm as they would locally. TETHER_DOCKER=local uses the
# Mac's own Docker instead (e.g. when devvm is down). Elsewhere (devvm itself,
# via `scripts/devvm.sh run`) the default is the local Docker.
#
# Exits non-zero if any step fails. Temporary files are removed on exit.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
# Where cargo puts build output (honours CARGO_TARGET_DIR, as cargo does).
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
export PATH="$HOME/.cargo/bin:$PATH"

# The core-crypto golden generators are #[ignore]d and inert unless this is
# set; --include-ignored must never rewrite the committed fixtures.
unset TETHER_REGEN_GOLDEN

CANARY_LABEL="CANARY-7f3a"
ALL_STEPS=(fixture cargo throughput vitest playwright server sync canary)
OPTIONAL_STEPS=(android-clippy android-apk deps)
if (($# > 0)); then
  STEPS=("$@")
  for s in "${STEPS[@]}"; do
    case " ${ALL_STEPS[*]} ${OPTIONAL_STEPS[*]} " in
      *" $s "*) ;;
      *) echo "unknown step: $s (steps: ${ALL_STEPS[*]}; optional: ${OPTIONAL_STEPS[*]})" >&2; exit 2 ;;
    esac
  done
else
  STEPS=("${ALL_STEPS[@]}")
fi

# Docker host for the steps that need one (see the header).
DEVVM_OPENED=0
needs_docker=0
for s in "${STEPS[@]}"; do
  case "$s" in fixture | cargo | throughput | server | canary) needs_docker=1 ;; esac
done
if ((needs_docker)); then
  default_docker=local
  [[ "$(uname -s)" == Darwin ]] && default_docker=devvm
  case "${TETHER_DOCKER:-$default_docker}" in
    local) echo "docker: local" ;;
    devvm)
      scripts/devvm.sh status 2>/dev/null || DEVVM_OPENED=1
      devvm_env="$(scripts/devvm.sh up)" || exit 1
      eval "$devvm_env"
      echo "docker: devvm via $DOCKER_HOST" ;;
    *) echo "TETHER_DOCKER must be devvm or local" >&2; exit 2 ;;
  esac
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/tether-acceptance.XXXXXX")"
LOGS="$WORK/logs"
mkdir -p "$LOGS"
CANARY_PID=""
SERVER_STARTED=0

# `docker compose` for the acceptance server: project tether-accept (never
# the user's own `tether` project), host port 18443 on loopback only (devvm
# is on the LAN), certificate for localhost, invites pointing at that port. The TLS and bind variables are
# cleared so a shell that exports them cannot change what is tested.
ACCEPT_PORT=18443
ACCEPT_URL="https://localhost:$ACCEPT_PORT"
compose() {
  env -u TETHER_TLS -u TETHER_TLS_CERT -u TETHER_TLS_KEY -u TETHER_ALLOW_PLAIN \
    -u TETHER_BIND -u TETHER_IPS -u COMPOSE_PROJECT_NAME \
    TETHER_PORT="127.0.0.1:$ACCEPT_PORT" TETHER_NAMES=localhost TETHER_URL="$ACCEPT_URL" \
    docker compose -f "$ROOT/server/compose.yml" -p tether-accept "$@"
}

cleanup() {
  # Only the tether-accept project, only what this run started; volumes go
  # too (-v) so no account data outlives the run.
  if ((SERVER_STARTED)); then
    echo "removing the tether-accept container and volume"
    compose down -v --remove-orphans >/dev/null 2>&1 || true
  fi
  if [[ -n "$CANARY_PID" ]] && kill -0 "$CANARY_PID" 2>/dev/null; then
    kill "$CANARY_PID" 2>/dev/null || true
    wait "$CANARY_PID" 2>/dev/null || true
  fi
  rm -rf "$WORK"
  if ((DEVVM_OPENED)); then scripts/devvm.sh down || true; fi
}
trap cleanup EXIT
trap 'exit 130' INT TERM

SUMMARY=()
FAILED=0

section() {
  printf '\n==================== %s ====================\n' "$1"
}

# record <step> <PASS|FAIL> <detail>
record() {
  SUMMARY+=("$(printf '%-11s %-4s  %s' "$1" "$2" "$3")")
  if [[ "$2" == FAIL ]]; then FAILED=1; fi
  printf -- '--> %s: %s (%s)\n' "$1" "$2" "$3"
}

# run_logged <log> <cmd...>: runs the command with output to the terminal and
# the log; returns its exit status.
run_logged() {
  local log="$1"
  shift
  local rc=0
  # pipefail: a failing command fails the pipeline (tee itself succeeds).
  "$@" 2>&1 | tee "$log" || rc=$?
  return "$rc"
}

want() {
  case " ${STEPS[*]} " in *" $1 "*) return 0 ;; *) return 1 ;; esac
}

# Occurrences of fixed string $1 in file $2 (0 for a missing file).
occurrences() {
  if [[ -e "$2" ]]; then
    { LC_ALL=C grep -a -o -F -- "$1" "$2" || true; } | wc -l | tr -d ' '
  else
    echo 0
  fi
}

# --- fixture -----------------------------------------------------------------
if want fixture; then
  section "SSH fixture (tests/fixtures/up.sh)"
  if run_logged "$LOGS/fixture.log" ./tests/fixtures/up.sh; then
    record fixture PASS "sshd ready on 127.0.0.1:2222"
  else
    record fixture FAIL "up.sh failed; fixture-backed tests below will fail too"
  fi
fi

# --- cargo -------------------------------------------------------------------
# Sums libtest "test result:" lines: "<passed> passed, <failed> failed, <ignored> ignored, <binaries> binaries".
cargo_counts() {
  awk '/^test result:/ {
         for (i = 1; i <= NF; i++) {
           if ($(i+1) ~ /^passed;?$/) p += $i
           if ($(i+1) ~ /^failed;?$/) f += $i
           if ($(i+1) ~ /^ignored;?$/) g += $i
         }
         b++
       }
       END { printf "%d passed, %d failed, %d ignored, %d test binaries", p, f, g, b }' "$1"
}

if want cargo; then
  section "Rust workspace tests (fixture on, --include-ignored)"
  # shell_cat_throughput is an #[ignore]d benchmark with no assertion beyond
  # "finishes within 120 s"; a debug build says nothing about throughput and
  # may hit that timeout. It runs in release in the `throughput` step instead.
  echo "note: skipping shell_cat_throughput here (debug-meaningless benchmark; runs in release in the 'throughput' step)"
  echo "note: core-crypto golden generators run but are inert (TETHER_REGEN_GOLDEN unset)"
  cargo_skips=(--skip shell_cat_throughput)
  # --no-fail-fast: one failing test binary must not hide the others' results.
  if run_logged "$LOGS/cargo.log" \
    env TETHER_SSH_FIXTURE=1 cargo test --workspace --no-fail-fast -- --include-ignored "${cargo_skips[@]}"; then
    record cargo PASS "$(cargo_counts "$LOGS/cargo.log")"
  else
    record cargo FAIL "$(cargo_counts "$LOGS/cargo.log"); see failures above"
  fi
  if grep -q "upload_1gb_checksum ... ok" "$LOGS/cargo.log"; then
    echo "1 GiB SFTP checksum (upload_1gb_checksum): ran and passed"
  else
    record cargo-1gib FAIL "upload_1gb_checksum did not report ok"
  fi
fi

# --- throughput (headless, release) -------------------------------------------
if want throughput; then
  section "Headless shell throughput (release; informational, not the GUI gate)"
  if run_logged "$LOGS/throughput.log" \
    env TETHER_SSH_FIXTURE=1 cargo test --release -p core-ssh --test throughput -- --ignored --nocapture; then
    line="$(grep -o 'shell_cat_throughput: .*' "$LOGS/throughput.log" | tail -1 || true)"
    if [[ -n "$line" ]]; then
      record throughput PASS "${line#shell_cat_throughput: } (headless SSH side only; GUI gate is manual)"
    else
      record throughput FAIL "benchmark printed no result (fixture off?)"
    fi
  else
    record throughput FAIL "benchmark failed"
  fi
fi

# --- vitest ------------------------------------------------------------------
if want vitest; then
  section "Frontend unit tests (vitest)"
  if run_logged "$LOGS/vitest.log" pnpm -C apps/tauri vitest run; then
    record vitest PASS "$(grep -E '^ *Tests ' "$LOGS/vitest.log" | tail -1 | sed -E 's/\x1b\[[0-9;]*m//g; s/^ *Tests +//')"
  else
    record vitest FAIL "$(grep -E '^ *Tests ' "$LOGS/vitest.log" | tail -1 | sed -E 's/\x1b\[[0-9;]*m//g; s/^ *Tests +//' || true)"
  fi
fi

# --- playwright --------------------------------------------------------------
if want playwright; then
  section "Frontend end-to-end tests (playwright, mock backend)"
  if run_logged "$LOGS/playwright.log" pnpm -C apps/tauri playwright test; then
    record playwright PASS "$(grep -E '^ *[0-9]+ passed' "$LOGS/playwright.log" | tail -1 | sed -E 's/^ *//')"
  else
    record playwright FAIL "$(grep -E '^ *[0-9]+ (passed|failed)' "$LOGS/playwright.log" | tr '\n' ' ' | sed -E 's/ +/ /g' || true)"
  fi
fi

# --- server ------------------------------------------------------------------
# server_up: builds and starts the acceptance container once per run and sets
# SERVER_PIN to what `termlyx-server fingerprint` prints. Idempotent: the
# server and canary steps share the container. Returns non-zero (and records
# why in SERVER_ERR) when it cannot be started.
SERVER_STATE=""   # "", "up" or "failed"
SERVER_PIN=""
SERVER_ERR=""
server_up() {
  [[ "$SERVER_STATE" == up ]] && return 0
  [[ "$SERVER_STATE" == failed ]] && return 1
  SERVER_STATE=failed
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    SERVER_ERR="docker is not running"
    return 1
  fi
  # From here on the trap removes the project, even if the start fails.
  SERVER_STARTED=1
  # A leftover from a killed earlier run would carry old accounts.
  compose down -v --remove-orphans >/dev/null 2>&1 || true
  if ! run_logged "$LOGS/server-up.log" compose up -d --build; then
    SERVER_ERR="docker compose up --build failed"
    return 1
  fi
  local waited=0
  until compose exec -T tether termlyx-server health >/dev/null 2>&1; do
    if ((waited >= 120)); then
      SERVER_ERR="the server did not become healthy within 120 s"
      compose logs --no-color --tail 40 tether || true
      return 1
    fi
    sleep 1
    waited=$((waited + 1))
  done
  SERVER_PIN="$(compose exec -T tether termlyx-server fingerprint | head -n 1 | tr -d '\r')"
  if [[ ! "$SERVER_PIN" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    SERVER_ERR="unexpected fingerprint output"
    SERVER_PIN=""
    return 1
  fi
  echo "server: healthy after ${waited}s at $ACCEPT_URL, pin $SERVER_PIN"
  SERVER_STATE=up
}

# new_invite: prints a fresh single-use invite code.
new_invite() {
  local code
  code="$(compose exec -T tether termlyx-server invite | tr -d '\r' | tail -n 1)"
  [[ "$code" == tether1-* ]] || return 1
  printf '%s\n' "$code"
}

if want server; then
  section "Real tether-server container + two vaults syncing through it"
  started=$SECONDS
  if ! server_up; then
    record server FAIL "$SERVER_ERR"
  else
    if invite="$(new_invite)"; then
      if run_logged "$LOGS/server.log" \
        env TETHER_SYNC_SERVER="$ACCEPT_URL" TETHER_SYNC_PIN="$SERVER_PIN" TETHER_SYNC_INVITE="$invite" \
        cargo test -p core-sync --test e2e_docker -- --include-ignored --nocapture; then
        if grep -q "two_vaults_sync_through_real_server ... ok" "$LOGS/server.log" \
          && ! grep -q "skipping two_vaults_sync_through_real_server" "$LOGS/server.log"; then
          ab="$(grep -o 'A -> B live update: .*' "$LOGS/server.log" | tail -1 | sed 's/.*: //' || true)"
          ba="$(grep -o 'B -> A live update: .*' "$LOGS/server.log" | tail -1 | sed 's/.*: //' || true)"
          record server PASS "two_vaults_sync_through_real_server ok in $((SECONDS - started)) s incl. image build (container tether-accept, pin ${SERVER_PIN:0:19}...; live update A->B $ab, B->A $ba)"
        else
          record server FAIL "the test passed without running (skipped?)"
        fi
      else
        record server FAIL "two_vaults_sync_through_real_server failed; see above"
      fi
    else
      record server FAIL "could not create an invite"
    fi
  fi
fi

# --- sync (simulation) ----------------------------------------------------------
if want sync; then
  section "Sync convergence simulation (200 seeds)"
  started=$SECONDS
  if run_logged "$LOGS/sync.log" \
    env TETHER_SIM_SEEDS=200 cargo test -p core-sync --test simulation; then
    record sync PASS "$(cargo_counts "$LOGS/sync.log"); 200 seeds in $((SECONDS - started)) s"
  else
    record sync FAIL "$(cargo_counts "$LOGS/sync.log"); see failures above"
  fi
fi

# --- canary ------------------------------------------------------------------
# grep_vault <phase> <data-dir> <needle-name>=<needle> ...: prints a count
# table for the vault file, WAL, shm and the canary log; returns the total.
CANARY_TOTAL=0
grep_vault() {
  local phase="$1" dir="$2"
  shift 2
  local files=("$dir/vault.sqlite" "$dir/vault.sqlite-wal" "$dir/vault.sqlite-shm" "$LOGS/canary.log")
  echo "grep ($phase):"
  local f size
  for f in "${files[@]}"; do
    if [[ -e "$f" ]]; then size="$(wc -c <"$f" | tr -d ' ') bytes"; else size="absent"; fi
    printf '  %-20s %s\n' "$(basename "$f")" "$size"
  done
  local pair name needle n line
  for pair in "$@"; do
    name="${pair%%=*}"
    needle="${pair#*=}"
    line="$(printf '  %-20s' "$name")"
    for f in "${files[@]}"; do
      n="$(occurrences "$needle" "$f")"
      CANARY_TOTAL=$((CANARY_TOTAL + n))
      line+="$(printf ' %s=%s' "$(basename "$f" | sed 's/^vault\.sqlite/db/')" "$n")"
    done
    echo "$line"
  done
}

# capture_server_files <dir>: copies what the acceptance container holds on
# disk and in its logs into <dir>: the database, its WAL (if there is one), a
# consistent backup made by `termlyx-server backup`, and `compose logs`.
capture_server_files() {
  local dir="$1"
  mkdir -p "$dir"
  compose exec -T tether rm -f /data/b.db
  compose exec -T tether termlyx-server backup /data/b.db >"$LOGS/server-backup.log" 2>&1 || {
    cat "$LOGS/server-backup.log" >&2
    return 1
  }
  compose cp tether:/data/b.db "$dir/b.db" >"$LOGS/server-cp.log" 2>&1 || {
    cat "$LOGS/server-cp.log" >&2
    return 1
  }
  compose cp tether:/data/tether.db "$dir/tether.db" >"$LOGS/server-cp.log" 2>&1 || {
    cat "$LOGS/server-cp.log" >&2
    return 1
  }
  # A WAL exists while the server has the database open and uncheckpointed.
  compose cp tether:/data/tether.db-wal "$dir/tether.db-wal" >/dev/null 2>&1 || true
  compose logs --no-color --no-log-prefix tether >"$dir/server.log" 2>&1 || return 1
  compose exec -T tether rm -f /data/b.db
}

# grep_server <phase> <dir> <name>=<needle> ...: the count table for the
# server's files, the same shape as grep_vault; adds to CANARY_TOTAL.
SERVER_FILES=(tether.db tether.db-wal b.db server.log)
grep_server() {
  local phase="$1" dir="$2"
  shift 2
  echo "grep ($phase):"
  local f size
  for f in "${SERVER_FILES[@]}"; do
    if [[ -e "$dir/$f" ]]; then size="$(wc -c <"$dir/$f" | tr -d ' ') bytes"; else size="absent"; fi
    printf '  %-20s %s\n' "$f" "$size"
  done
  local pair name needle n line
  for pair in "$@"; do
    name="${pair%%=*}"
    needle="${pair#*=}"
    line="$(printf '  %-20s' "$name")"
    for f in "${SERVER_FILES[@]}"; do
      n="$(occurrences "$needle" "$dir/$f")"
      CANARY_TOTAL=$((CANARY_TOTAL + n))
      line+="$(printf ' %s=%s' "$(echo "$f" | sed 's/^tether\.db/db/; s/^b\.db/backup/; s/^server\.log/logs/')" "$n")"
    done
    echo "$line"
  done
}

if want canary; then
  section "Canary: no plaintext in the vault file, WAL, shm, the server's database, backup or logs"
  echo "builds and runs apps/tauri/src-tauri/examples/acceptance_canary.rs: AppState"
  echo "vault_create/lock/unlock, key_generate x2, upsert_entity identity+host,"
  echo "list_entities, key_export_private, a prompt-saved password"
  echo "(save_host_password) on a real vault file (production KDF), then sync_enable"
  echo "against the tether-accept container and a wait for the first sync."
  echo "Not covered: the webview -> IPC hop."
  canary_ok=1
  if ! run_logged "$LOGS/canary-build.log" cargo build -p tether-app --example acceptance_canary; then
    record canary FAIL "example did not build"
    canary_ok=0
  fi
  if ((canary_ok)) && ! server_up; then
    record canary FAIL "the sync server container did not start: $SERVER_ERR"
    canary_ok=0
  fi
  sync_invite=""
  if ((canary_ok)); then
    if ! sync_invite="$(new_invite)"; then
      record canary FAIL "could not create an invite"
      canary_ok=0
    fi
  fi
  if ((canary_ok)); then
    data="$WORK/app-data"
    hs="$WORK/handshake"
    mkdir -p "$data" "$hs"
    # Fresh secrets per run (the canary label is fixed).
    rnd() { LC_ALL=C tr -dc 'a-z0-9' </dev/urandom | head -c 12 || true; }
    master_pw="master-$(rnd)"
    identity_pw="idpw-$(rnd)"
    key_pass="keypass-$(rnd)"
    saved_pw="savedpw-$(rnd)"
    sync_user="canary-$(rnd)"
    # The invite code is a secret too (the server keeps only its hash).
    needles=(
      "canary=$CANARY_LABEL"
      "master_password=$master_pw"
      "identity_password=$identity_pw"
      "key_passphrase=$key_pass"
      "saved_password=$saved_pw"
      "invite_code=$sync_invite"
    )
    # The server learns the account name and record routing metadata; the
    # same needles must not reach its files.
    server_needles=("${needles[@]}")
    server_dir="$WORK/server-files"
    started=$SECONDS
    TETHER_CANARY_LABEL="$CANARY_LABEL" \
      TETHER_CANARY_MASTER_PW="$master_pw" \
      TETHER_CANARY_IDENTITY_PW="$identity_pw" \
      TETHER_CANARY_KEY_PASSPHRASE="$key_pass" \
      TETHER_CANARY_SAVED_PW="$saved_pw" \
      TETHER_CANARY_SYNC_INVITE="$sync_invite" \
      TETHER_CANARY_SYNC_USER="$sync_user" \
      RUST_LOG=trace \
      "$TARGET_DIR/debug/examples/acceptance_canary" "$data" "$hs" "$server_dir" >"$LOGS/canary.log" 2>&1 &
    CANARY_PID=$!
    # Phase 1: wait until everything is stored and the vault is still open.
    while [[ ! -e "$hs/open" ]] && kill -0 "$CANARY_PID" 2>/dev/null; do
      if ((SECONDS - started > 600)); then
        echo "canary did not reach the open state within 600 s" >&2
        break
      fi
      sleep 0.2
    done
    if [[ -e "$hs/open" ]]; then
      if [[ ! -s "$data/vault.sqlite-wal" ]]; then
        echo "note: the WAL is empty or absent while the vault is open"
      fi
      # Positive controls: grep reads these binary files, and each needle is
      # found when it is present.
      header="$(occurrences "SQLite format 3" "$data/vault.sqlite")"
      echo "positive control: 'SQLite format 3' found $header time(s) in vault.sqlite"
      if ((header < 1)); then canary_ok=0; fi
      for pair in "${needles[@]}"; do
        printf '\0\1%s\0' "${pair#*=}" >"$WORK/control.bin"
        if [[ "$(occurrences "${pair#*=}" "$WORK/control.bin")" != 1 ]]; then
          echo "positive control failed for ${pair%%=*}" >&2
          canary_ok=0
        fi
      done
      rm -f "$WORK/control.bin"
      echo "positive control: each needle is found once in a binary control file"
      grep_vault "vault open, WAL not checkpointed" "$data" "${needles[@]}"
      # The first sync has finished (the program waited for it): look at what
      # the server holds.
      if capture_server_files "$server_dir"; then
        server_ctl_ok=1
        # Positive controls: the account name is plaintext metadata the server
        # keeps, so it must be found in the database copy and the backup; the
        # logs must show the push (route names are logged, never bodies).
        n_db="$(occurrences "$sync_user" "$server_dir/tether.db")"
        n_wal="$(occurrences "$sync_user" "$server_dir/tether.db-wal")"
        n_bak="$(occurrences "$sync_user" "$server_dir/b.db")"
        echo "positive control: account name found $n_db time(s) in the database, $n_wal in its WAL, $n_bak in the backup"
        if ((n_db + n_wal < 1 || n_bak < 1)); then server_ctl_ok=0; fi
        n="$(occurrences "/v1/sync/push" "$server_dir/server.log")"
        echo "positive control: '/v1/sync/push' found $n time(s) in the server log"
        if ((n < 1)); then server_ctl_ok=0; fi
        n="$(occurrences "SQLite format 3" "$server_dir/b.db")"
        echo "positive control: 'SQLite format 3' found $n time(s) in the backup"
        if ((n < 1)); then server_ctl_ok=0; fi
        if ((server_ctl_ok == 0)); then
          echo "a server positive control failed" >&2
          canary_ok=0
        fi
        grep_server "server, after the first sync" "$server_dir" "${server_needles[@]}"
      else
        echo "could not collect the server's files" >&2
        canary_ok=0
      fi
      touch "$hs/release"
    else
      canary_ok=0
    fi
    rc=0
    wait "$CANARY_PID" || rc=$?
    CANARY_PID=""
    echo "--- canary program output ---"
    cat "$LOGS/canary.log"
    echo "--- end ---"
    if ((rc != 0)); then
      echo "canary program exited with $rc" >&2
      canary_ok=0
    fi
    # Phase 2: after the state was dropped (and the vault reopened and closed).
    grep_vault "after close" "$data" "${needles[@]}"
    mem_line="$(grep -o 'in-process secret matches: [0-9]*' "$LOGS/canary.log" | tail -1 || true)"
    if ((canary_ok)) && ((CANARY_TOTAL == 0)) && [[ "$mem_line" == "in-process secret matches: 0" ]]; then
      record canary PASS "0 matches for ${#needles[@]} needles x 4 vault files (db, wal, shm, log), open and after close; ${#server_needles[@]} needles x 4 server files (db, wal, backup, logs); $mem_line"
    else
      record canary FAIL "matches: $CANARY_TOTAL; program ok: $canary_ok (exit $rc); ${mem_line:-no in-process result}"
    fi
  fi
fi

# --- android (optional) ----------------------------------------------------------
# Android toolchain locations (devvm's ~/.profile values; not sourced over a
# plain ssh command). The NDK is the newest one installed.
android_env() {
  export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/sdk}"
  export JAVA_HOME="${JAVA_HOME:-/usr/lib/jvm/java-17-openjdk-amd64}"
  export NDK_HOME="${NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -n 1)}"
  [[ -d "$NDK_HOME" ]] || { echo "no Android NDK under $ANDROID_HOME/ndk" >&2; return 1; }
  export PATH="$JAVA_HOME/bin:$PATH"
  export CARGO_INCREMENTAL=0
  # rust-toolchain.toml pins an exact version, which starts without these targets.
  (cd "$ROOT" && rustup target add x86_64-linux-android aarch64-linux-android) || return 1
}

if want android-clippy; then
  section "Android clippy (x86_64 + aarch64, API 30)"
  if android_env; then
    bin="$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
    export "AR_x86_64_linux_android=$bin/llvm-ar" "AR_aarch64_linux_android=$bin/llvm-ar"
    export "CC_x86_64_linux_android=$bin/x86_64-linux-android30-clang"
    export "CC_aarch64_linux_android=$bin/aarch64-linux-android30-clang"
    export "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER=$bin/x86_64-linux-android30-clang"
    export "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$bin/aarch64-linux-android30-clang"
    clippy_ok=1
    for t in x86_64-linux-android aarch64-linux-android; do
      run_logged "$LOGS/android-clippy-$t.log" cargo clippy -p tether-app --target "$t" -- -D warnings || clippy_ok=0
    done
    if ((clippy_ok)); then
      record android-clippy PASS "tether-app clean for x86_64 and aarch64 Android"
    else
      record android-clippy FAIL "clippy failed; see above"
    fi
  else
    record android-clippy FAIL "Android NDK not found"
  fi
fi

if want android-apk; then
  section "Android APK (tauri android build)"
  if android_env; then
    # Ignored, so `devvm.sh sync` keeps it: a release build's arm64 libs must not enter a debug APK.
    rm -rf "$ROOT/apps/tauri/src-tauri/gen/android/app/src/main/jniLibs"
    # AGP updates an existing APK in place and leaves the old bytes behind (612M vs 314M).
    rm -rf "$ROOT/apps/tauri/src-tauri/gen/android/app/build/outputs/apk" "$ROOT/apps/tauri/src-tauri/gen/android/app/build/outputs/bundle"
    if [[ "${TETHER_ANDROID_AAB:-}" == 1 ]]; then
      vc="${TETHER_ANDROID_VERSION_CODE:-}"
      [[ "$vc" =~ ^[1-9][0-9]*$ ]] || { echo "TETHER_ANDROID_VERSION_CODE must be a positive integer, got '$vc'" >&2; vc=""; }
      apk_args=(--target aarch64 --aab --config "{\"bundle\":{\"android\":{\"versionCode\":${vc:-0}}}}")
    elif [[ "${TETHER_ANDROID_RELEASE:-}" == 1 ]]; then
      apk_args=(--target aarch64 --apk)
      apk_glob="release"
    else
      apk_args=(--debug --target "${TETHER_ANDROID_TARGET:-x86_64}" --apk)
      apk_glob="debug"
    fi
    props="$ROOT/apps/tauri/src-tauri/gen/android/app/tauri.properties"
    if [[ "${TETHER_ANDROID_AAB:-}" == 1 ]]; then
      if [[ -z "$vc" ]]; then
        record android-apk FAIL "no valid TETHER_ANDROID_VERSION_CODE"
      elif ! run_logged "$LOGS/android-apk.log" pnpm -C apps/tauri tauri android build "${apk_args[@]}"; then
        record android-apk FAIL "tauri android build --aab failed; see above"
      elif ! grep -qx "tauri.android.versionCode=$vc" "$props" 2>/dev/null; then
        # The versionCode gradle used comes from this file; a stale one would be rejected by Play.
        record android-apk FAIL "tauri.properties does not have versionCode=$vc: $(grep versionCode "$props" 2>/dev/null || echo missing)"
      else
        aab="$(ls -t "$ROOT"/apps/tauri/src-tauri/gen/android/app/build/outputs/bundle/universalRelease/*.aab 2>/dev/null | head -n 1 || true)"
        if [[ -n "$aab" ]]; then
          echo "AAB: $aab"
          record android-apk PASS "$aab ($(du -h "$aab" | cut -f1), versionCode $vc)"
        else
          record android-apk FAIL "build succeeded but no AAB under outputs/bundle/universalRelease"
        fi
      fi
    elif run_logged "$LOGS/android-apk.log" pnpm -C apps/tauri tauri android build "${apk_args[@]}"; then
      apk="$(ls -t "$ROOT"/apps/tauri/src-tauri/gen/android/app/build/outputs/apk/universal/"$apk_glob"/*.apk 2>/dev/null | head -n 1 || true)"
      if [[ -n "$apk" ]]; then
        echo "APK: $apk"
        record android-apk PASS "$apk ($(du -h "$apk" | cut -f1))"
      else
        record android-apk FAIL "build succeeded but no APK under outputs/apk/universal/$apk_glob"
      fi
    else
      record android-apk FAIL "tauri android build failed; see above"
    fi
  else
    record android-apk FAIL "Android NDK not found"
  fi
fi

if want deps; then
  section "deps"
  if ! command -v cargo-deny >/dev/null; then
    record deps FAIL "cargo-deny not installed (cargo install --locked cargo-deny)"
  elif cargo deny --all-features check licenses advisories bans sources &&
    pnpm -C apps/tauri install --frozen-lockfile &&
    pnpm -C apps/tauri audit --prod --audit-level high &&
    node scripts/release/npm-licenses.mjs; then
    record deps PASS "cargo-deny, npm audit, npm licenses clean"
  else
    record deps FAIL "dependency policy check failed; see above"
  fi
fi

# --- summary -----------------------------------------------------------------
section "Summary"
for line in ${SUMMARY[@]+"${SUMMARY[@]}"}; do echo "$line"; done
if ((FAILED)); then
  echo "RESULT: FAIL"
  exit 1
fi
echo "RESULT: PASS (${#SUMMARY[@]} steps)"
