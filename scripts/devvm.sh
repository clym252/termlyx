#!/usr/bin/env bash
# Mac -> devvm: run Tether's Linux-runnable tests there, or tunnel to its Docker.
# See tests/fixtures/README.md.
#
# The heavy gate runs ON devvm (its own cargo build cache and Docker), the Mac
# only sends the commit and shows the output:
#   scripts/devvm.sh sync            send committed HEAD to devvm (~/src/termlyx) and check it out
#   scripts/devvm.sh run <step>...   scripts/acceptance.sh <step>... on devvm; output
#                                    streamed and saved to $CARGO_TARGET_DIR/devvm-logs/
#
# For anything that must still run on the Mac against the containers, a tunnel.
# The SSH fixture (tether-sshd) and the acceptance sync server (tether-accept)
# run in Docker on the Linux host devvm instead of Docker Desktop on the Mac.
# One background SSH connection carries everything:
#   localhost:2222   -> devvm 127.0.0.1:2222   (fixture sshd)
#   localhost:18443  -> devvm 127.0.0.1:18443  (acceptance sync server)
#   $DOCKER_SOCK     -> devvm /var/run/docker.sock
# With DOCKER_HOST=unix://$DOCKER_SOCK the Mac's docker CLI (compose, exec,
# cp, image builds) drives devvm's daemon, so the tests stay unchanged.
#
#   eval "$(scripts/devvm.sh up)"   open the tunnel (no-op if open) and export DOCKER_HOST
#   scripts/devvm.sh env            print the DOCKER_HOST export line
#   scripts/devvm.sh status         exit 0 if the tunnel is open
#   scripts/devvm.sh down           close the tunnel (containers on devvm keep running)
#
# Env:
#   TETHER_DEVVM_HOST   ssh target, user@host (required)
#   TETHER_DEVVM_KEY    ssh key (default ~/.ssh/id_ed25519)
#   CARGO_TARGET_DIR    local dir for devvm-logs/ (default target/)
#
# Messages go to stderr; up/env print only the export line on stdout.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ROOT/scripts/local-config.sh"

HOST="${TETHER_DEVVM_HOST:?set TETHER_DEVVM_HOST (user@host of the Linux build VM)}"
KEY="${TETHER_DEVVM_KEY:-$HOME/.ssh/id_ed25519}"
# Unix socket paths are limited to 104 bytes on macOS: keep this short.
STATE_DIR="$HOME/.cache/tether-devvm"
CTL="$STATE_DIR/ctl"
DOCKER_SOCK="$STATE_DIR/docker.sock"

SSH_OPTS=(-i "$KEY" -o IdentitiesOnly=yes -o BatchMode=yes -o ConnectTimeout=10
  -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -S "$CTL")

usage() {
  sed -n '2,31p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

is_up() { ssh "${SSH_OPTS[@]}" -O check "$HOST" >/dev/null 2>&1; }

print_env() { printf 'export DOCKER_HOST=unix://%s\n' "$DOCKER_SOCK"; }

up() {
  if is_up; then
    echo "devvm tunnel already open ($HOST)" >&2
    print_env
    return 0
  fi
  mkdir -p "$STATE_DIR"
  chmod 700 "$STATE_DIR"
  rm -f "$CTL"
  # -M -f -N: a backgrounded control master holding only the forwards.
  # ExitOnForwardFailure: fail now if 2222 or 18443 is taken on the Mac (the
  # Mac's own Docker fixture still up?). StreamLocalBindUnlink: replace a
  # stale socket from a killed tunnel. The socket is created 0600 in a 0700
  # directory: only this user can reach devvm's Docker through it.
  if ! ssh "${SSH_OPTS[@]}" -M -f -N \
    -o ExitOnForwardFailure=yes -o StreamLocalBindUnlink=yes \
    -L 2222:127.0.0.1:2222 \
    -L 18443:127.0.0.1:18443 \
    -L "$DOCKER_SOCK:/var/run/docker.sock" \
    "$HOST"; then
    echo "error: could not open the devvm tunnel to $HOST" >&2
    echo "  (devvm down? port 2222 or 18443 already in use on the Mac?)" >&2
    echo "  To use the Mac's own Docker instead: TETHER_DOCKER=local" >&2
    return 1
  fi
  if ! DOCKER_HOST="unix://$DOCKER_SOCK" docker version --format '{{.Server.Version}}' >/dev/null 2>&1; then
    echo "error: tunnel open, but devvm's Docker does not answer through it" >&2
    ssh "${SSH_OPTS[@]}" -O exit "$HOST" >/dev/null 2>&1 || true
    return 1
  fi
  echo "devvm tunnel open ($HOST): localhost:2222, localhost:18443, docker at $DOCKER_SOCK" >&2
  print_env
}

down() {
  if is_up; then
    ssh "${SSH_OPTS[@]}" -O exit "$HOST" >/dev/null 2>&1 || true
    echo "devvm tunnel closed" >&2
  else
    echo "devvm tunnel not open" >&2
  fi
  rm -f "$DOCKER_SOCK"
}

# Remote checkout (paths expanded by devvm's shell).
# shellcheck disable=SC2016
VM_REPO='$HOME/src/termlyx'
# shellcheck disable=SC2016
VM_BUNDLE='$HOME/src/sync.bundle'
# Plain session (no control socket): the tunnel is not needed for sync/run.
VM_SSH=(ssh -i "$KEY" -o IdentitiesOnly=yes -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=30)
# shellcheck disable=SC2029 # expanded on the Mac on purpose
vm() { "${VM_SSH[@]}" -n "$HOST" "$@"; }

sync_vm() {
  cd "$ROOT"
  if ! git diff --quiet HEAD --; then
    echo "uncommitted tracked changes: commit first (a WIP commit is fine)" >&2
    exit 1
  fi
  local sha tmp vm_sha
  sha="$(git rev-parse HEAD)"
  tmp="$(mktemp -d)"
  git bundle create "$tmp/sync.bundle" HEAD 2>&1 | tail -n1
  vm "mkdir -p \$HOME/src"
  "${VM_SSH[@]}" "$HOST" "cat > $VM_BUNDLE" <"$tmp/sync.bundle"
  rm -rf "$tmp"
  # `git clean -fd` (no -x) keeps target/ and node_modules/.
  vm "set -e; [ -d $VM_REPO/.git ] || git init -q $VM_REPO; cd $VM_REPO && git fetch -q $VM_BUNDLE HEAD && git checkout -q -f --detach FETCH_HEAD && git clean -fdq"
  vm_sha="$(vm "git -C $VM_REPO rev-parse HEAD")"
  if [[ "$vm_sha" != "$sha" ]]; then
    echo "sync failed: devvm at '$vm_sha', expected $sha" >&2
    exit 1
  fi
  echo "synced $sha to $HOST:~/src/termlyx" >&2
}

run_vm() {
  [[ $# -gt 0 ]] || usage
  local log_dir="${CARGO_TARGET_DIR:-$ROOT/target}/devvm-logs" vm_sha local_sha q=""
  mkdir -p "$log_dir"
  local_sha="$(git -C "$ROOT" rev-parse HEAD)"
  vm_sha="$(vm "git -C $VM_REPO rev-parse HEAD" 2>/dev/null || true)"
  if [[ "$vm_sha" != "$local_sha" ]]; then
    echo "warning: devvm has ${vm_sha:0:9}, Mac HEAD is ${local_sha:0:9} (run 'scripts/devvm.sh sync')" >&2
  fi
  printf -v q '%q ' "$@"
  # devvm's own Docker (TETHER_DOCKER=local); CARGO_INCREMENTAL=0 keeps the
  # cache from growing with every synced commit (as on win-dev).
  local rc=0
  "${VM_SSH[@]}" -n "$HOST" \
    "cd $VM_REPO && PATH=\$HOME/.cargo/bin:\$PATH CARGO_INCREMENTAL=0 TETHER_DOCKER=local TETHER_ANDROID_RELEASE=${TETHER_ANDROID_RELEASE:-} TETHER_ANDROID_TARGET=${TETHER_ANDROID_TARGET:-} TETHER_ANDROID_AAB=${TETHER_ANDROID_AAB:-} TETHER_ANDROID_VERSION_CODE=${TETHER_ANDROID_VERSION_CODE:-} ./scripts/acceptance.sh $q" 2>&1 |
    tee "$log_dir/acceptance.log" || rc=$?
  echo "log: $log_dir/acceptance.log" >&2
  return "$rc"
}

case "${1:-}" in
  sync) [[ $# -eq 1 ]] || usage; sync_vm ;;
  run) shift; run_vm "$@" ;;
  up) up ;;
  down) down ;;
  status) if is_up; then echo "open ($HOST)" >&2; else echo "closed" >&2; exit 1; fi ;;
  env) print_env ;;
  *) usage ;;
esac
