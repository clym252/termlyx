#!/usr/bin/env bash
# Mac -> Windows VM build and test loop. See scripts/win/README.md.
#
# Usage:
#   scripts/win.sh sync              send committed HEAD to the VM and check it out
#   scripts/win.sh run <step>...     run steps on the VM, stream output, copy logs back
# Steps: cargo clippy guard tsc vitest playwright build bundle fixture
#   fixture runs last, in its own SSH session with reverse forwards (VM
#   loopback only) to the Mac's Docker SSH fixture, which is stopped after.
#   That fixture runs on devvm through scripts/devvm.sh unless TETHER_DOCKER=local.
#
# Env:
#   TETHER_WIN_HOST   ssh target, user@host (required)
#   TETHER_WIN_KEY    ssh key (default ~/.ssh/id_ed25519)
#   TETHER_DOCKER     devvm (default) or local: where the fixture's Docker runs
#   CARGO_TARGET_DIR  logs are copied to $CARGO_TARGET_DIR/win-logs/
#
# Exits non-zero if any step failed.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ROOT/scripts/local-config.sh"
cd "$ROOT"

HOST="${TETHER_WIN_HOST:?set TETHER_WIN_HOST (user@host of the Windows VM)}"
KEY="${TETHER_WIN_KEY:-$HOME/.ssh/id_ed25519}"
LOG_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/win-logs"
VM_REPO='C:\src\termlyx'
VM_LOGS='C:\src\logs'
VM_BUNDLE='C:\src\sync.bundle'
ALL_STEPS="cargo clippy guard tsc vitest playwright build bundle fixture"

SSH_OPTS=(-i "$KEY" -o IdentitiesOnly=yes -o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=30)

usage() {
  sed -n '2,18p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

# Arguments are expanded on the Mac on purpose; the VM's shell is cmd.
# shellcheck disable=SC2029
vm() { ssh -n "${SSH_OPTS[@]}" "$HOST" "$@"; }

# VM loopback port of the fixture step's docker relay (scripts/win/docker-relay.py).
RELAY_PORT=22375

TMP="$(mktemp -d)"
RELAY_PID=""
FIXTURE_UP=0
DEVVM_OPENED=0
cleanup() {
  stop_fixture
  rm -rf "$TMP"
}
# A signal waits for the running ssh to end, then exits through cleanup.
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

sync_vm() {
  if ! git diff --quiet HEAD --; then
    echo "uncommitted tracked changes: commit first (a WIP commit is fine)" >&2
    exit 1
  fi
  local sha vm_sha
  sha="$(git rev-parse HEAD)"
  git bundle create "$TMP/sync.bundle" HEAD 2>&1 | tail -n1
  # scp wants forward slashes and a leading slash before the drive letter.
  scp "${SSH_OPTS[@]}" -q "$TMP/sync.bundle" "$HOST:/${VM_BUNDLE//\\//}"
  vm "if not exist $VM_REPO\\.git git init -q $VM_REPO"
  # The VM checkout must match the Mac byte for byte (LF), so autocrlf is off there.
  # When that setting changes, existing CRLF files are rewritten after the checkout.
  local renorm=0 crlf
  crlf="$(vm "cd /d $VM_REPO && git config --get core.autocrlf" | tr -d '\r' || true)"
  if [[ "$crlf" != false ]]; then
    vm "cd /d $VM_REPO && git config core.autocrlf false"
    renorm=1
  fi
  # The VM's default shell is cmd. `git clean -fd` (no -x) keeps target and node_modules.
  vm "cd /d $VM_REPO && git fetch -q $VM_BUNDLE HEAD && git checkout -q -f --detach FETCH_HEAD && git clean -fdq"
  if ((renorm)); then
    echo "core.autocrlf was '${crlf:-unset}' on the VM; rewriting the working tree with LF endings"
    vm "cd /d $VM_REPO && git rm --cached -r -q . && git reset -q --hard"
  fi
  vm_sha="$(vm "cd /d $VM_REPO && git rev-parse HEAD" | tr -d '\r')"
  if [[ "$vm_sha" != "$sha" ]]; then
    echo "sync failed: VM at '$vm_sha', expected $sha" >&2
    exit 1
  fi
  echo "synced $sha to $HOST:$VM_REPO"
}

# Starts the Mac's Docker SSH fixture and the docker relay; stop_fixture (also
# run by cleanup on any exit) stops both.
start_fixture() {
  # Docker on devvm (scripts/devvm.sh): up.sh, down.sh and the relay all
  # inherit DOCKER_HOST; the Mac's localhost:2222 is the devvm tunnel.
  case "${TETHER_DOCKER:-devvm}" in
    local) ;;
    devvm)
      "$ROOT/scripts/devvm.sh" status 2>/dev/null || DEVVM_OPENED=1
      local devvm_env
      devvm_env="$("$ROOT/scripts/devvm.sh" up)" || exit 1
      eval "$devvm_env" ;;
    *) echo "TETHER_DOCKER must be devvm or local" >&2; exit 2 ;;
  esac
  FIXTURE_UP=1
  "$ROOT/tests/fixtures/up.sh"
  python3 "$ROOT/scripts/win/docker-relay.py" "$TMP/docker-relay.sock" "$ROOT" &
  RELAY_PID=$!
  local _
  for _ in $(seq 50); do
    [[ -S "$TMP/docker-relay.sock" ]] && return 0
    kill -0 "$RELAY_PID" 2>/dev/null || break
    sleep 0.1
  done
  echo "docker relay did not start" >&2
  exit 1
}

stop_fixture() {
  if [[ -n "$RELAY_PID" ]]; then
    kill "$RELAY_PID" 2>/dev/null || true
    wait "$RELAY_PID" 2>/dev/null || true
    RELAY_PID=""
  fi
  if ((FIXTURE_UP)); then
    FIXTURE_UP=0
    echo "stopping the SSH fixture (tests/fixtures/down.sh)"
    "$ROOT/tests/fixtures/down.sh" >/dev/null 2>&1 || echo "warning: tests/fixtures/down.sh failed" >&2
  fi
  if ((DEVVM_OPENED)); then
    DEVVM_OPENED=0
    "$ROOT/scripts/devvm.sh" down || true
  fi
}

# One SSH session running `steps` (comma-separated) through steps.ps1, with
# the extra ssh options in the remaining arguments. Output goes to the console
# and is appended to $TMP/run.out; then the session's step logs are copied
# back (before the next session's steps.ps1 deletes pnpm-install.log). Sets
# RC=1 on failure.
run_session() {
  local steps=$1 out="$TMP/session.out"
  shift
  ssh -n "${SSH_OPTS[@]}" "$@" "$HOST" \
    powershell -NoProfile -ExecutionPolicy Bypass -File "$VM_REPO\\scripts\\win\\steps.ps1" -Steps "$steps" |
    sed -l $'s/\r$//' | tee "$out" || RC=1
  cat "$out" >>"$TMP/run.out"

  # Copy the log of each step that reported a result; steps with none (the run
  # died first) must not leave an old local log looking fresh.
  local step log_steps=()
  IFS=, read -r -a log_steps <<<"$steps"
  grep -q '^pnpm-install exit=' "$out" && log_steps+=(pnpm-install)
  for step in "${log_steps[@]}"; do
    if grep -q "^$step exit=" "$out"; then
      if scp "${SSH_OPTS[@]}" -q "$HOST:/${VM_LOGS//\\//}/$step.log" "$LOG_DIR/$step.log" 2>/dev/null; then
        COPIED=1
      else
        echo "warning: could not copy $step.log" >&2
      fi
    else
      rm -f "$LOG_DIR/$step.log"
      echo "warning: no result for step '$step'; log not copied" >&2
      RC=1
    fi
  done
}

run_steps() {
  [[ $# -gt 0 ]] || usage
  local step vm_steps=() want_fixture=0
  for step in "$@"; do
    case " $ALL_STEPS " in
      *" $step "*) ;;
      *) echo "unknown step: $step (valid: $ALL_STEPS)" >&2; exit 2 ;;
    esac
    if [[ "$step" == fixture ]]; then want_fixture=1; else vm_steps+=("$step"); fi
  done

  mkdir -p "$LOG_DIR"
  : >"$TMP/run.out"
  RC=0
  COPIED=0
  if ((${#vm_steps[@]} > 0)); then
    run_session "$(IFS=,; echo "${vm_steps[*]}")"
  fi
  # fixture: the Mac's Docker fixture is reached from the VM through reverse
  # forwards bound to the VM's loopback. They, the relay and the fixture exist
  # only for this last session.
  if ((want_fixture)); then
    start_fixture
    run_session fixture -o ExitOnForwardFailure=yes \
      -R "127.0.0.1:2222:127.0.0.1:2222" \
      -R "127.0.0.1:$RELAY_PORT:$TMP/docker-relay.sock"
    stop_fixture
  fi
  ((COPIED)) && echo "logs: $LOG_DIR"

  local local_sha vm_sha
  local_sha="$(git rev-parse HEAD)"
  vm_sha="$(sed -n 's/^head=\([0-9a-f]*\) .*/\1/p' "$TMP/run.out" | head -n1)"
  if [[ -n "$vm_sha" && "$vm_sha" != "$local_sha" ]]; then
    echo "warning: VM ran ${vm_sha:0:9} but Mac HEAD is ${local_sha:0:9} (run 'scripts/win.sh sync')" >&2
  fi

  if ((RC != 0)) || grep -Eq '^[a-z-]+ exit=[1-9]' "$TMP/run.out"; then
    return 1
  fi
}

case "${1:-}" in
  sync) [[ $# -eq 1 ]] || usage; sync_vm ;;
  run) shift; run_steps "$@" ;;
  *) usage ;;
esac
