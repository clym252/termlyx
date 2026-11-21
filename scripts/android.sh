#!/usr/bin/env bash
# Mac -> devvm (builds) -> win-dev (emulator) Android loop. Nothing Android runs
# on the Mac: it only sends the commit and moves APKs.
#
# Usage:
#   scripts/android.sh build [debug|device|release|play]
#                                             sync HEAD to devvm, build the APK there
#                                             (acceptance step android-apk), copy it to
#                                             win-dev: ~\tether.alpha.apk (debug, x86_64),
#                                             ~\termlyx-device.apk (debug, arm64-v8a, for a real
#                                             phone or tablet over adb wireless debugging)
#                                             or ~\termlyx-release-unsigned.apk (release,
#                                             arm64-v8a). Start the emulator from RDP with
#                                             ~\tether-emulator.ps1, which installs the debug one.
#                                             play: the Play Store AAB (release, arm64-v8a,
#                                             versionCode = TETHER_BUILD_NUMBER or the commit
#                                             count; clean tree needed): ~\termlyx-play-unsigned.aab
#                                             (docs/store-release.md)
#   scripts/android.sh apk [dir]              copy the signed ~\termlyx-release.apk from win-dev to the
#                                             Mac (default dir: target/android/). The owner signs it first,
#                                             over RDP: scripts/win/android-sign.ps1 (scripts/win/README.md)
#   scripts/android.sh aab [dir]              copy the signed ~\termlyx-play.aab from win-dev to the Mac
#                                             (default dir: target/android/); signed first with
#                                             android-sign.ps1 -Aab over RDP
#   scripts/android.sh fixture up|down        SSH fixture (Docker on devvm) reachable from the
#                                             emulator at 10.0.2.2:2222 until `down`; never
#                                             overlap acceptance.sh / win.sh run fixture (same container)
#   scripts/android.sh logcat [lines]         last lines (default 300) of the emulator's log
#   scripts/android.sh shot [file.png]        emulator screenshot, copied to the Mac
#
# logcat and shot run adb on win-dev inside a 20 s PowerShell Start-Job (adb over
# SSH has hung before). On a timeout the PowerShell is printed, to run over RDP.
#
# Env:
#   TETHER_DEVVM_HOST, TETHER_DEVVM_KEY  as for scripts/devvm.sh
#   TETHER_WIN_HOST, TETHER_WIN_KEY      as for scripts/win.sh
#   ANDROID_SERIAL                       adb device for logcat/shot (e.g. the tablet's ip:port)
#                                        when more than one is connected

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ROOT/scripts/local-config.sh"

DEVVM_HOST="${TETHER_DEVVM_HOST:?set TETHER_DEVVM_HOST (user@host of the Linux build VM)}"
WIN_HOST="${TETHER_WIN_HOST:?set TETHER_WIN_HOST (user@host of the Windows VM)}"
DEVVM_KEY="${TETHER_DEVVM_KEY:-$HOME/.ssh/id_ed25519}"
WIN_KEY="${TETHER_WIN_KEY:-$HOME/.ssh/id_ed25519}"
STATE_DIR="$HOME/.cache/tether-android"
# Ownership markers (not the live ssh socket, which can die): what `fixture down` must clean.
OPENED_TUNNEL="$STATE_DIR/opened-devvm-tunnel" # `fixture up` opened the devvm tunnel
OWNS_FIXTURE="$STATE_DIR/owns-fixture"         # `fixture up` started the tether-sshd container
CTL="$STATE_DIR/ctl" # ssh control socket of the fixture forward (short path: macOS limit)

DEVVM_OPTS=(-i "$DEVVM_KEY" -o IdentitiesOnly=yes -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=30)
WIN_OPTS=(-i "$WIN_KEY" -o IdentitiesOnly=yes -o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=3)

usage() {
  sed -n '2,38p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

build() {
  local kind="${1:-debug}" win_name
  case "$kind" in
    debug) win_name=tether.alpha.apk ;;
    device) win_name=termlyx-device.apk ;;
    release) win_name=termlyx-release-unsigned.apk ;;
    play) win_name=termlyx-play-unsigned.aab ;;
    *) usage ;;
  esac
  [[ $# -le 1 ]] || usage

  unset TETHER_ANDROID_AAB TETHER_ANDROID_VERSION_CODE
  if [[ "$kind" == play ]]; then
    if [[ -n "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ]]; then
      echo "uncommitted changes: commit first, so the versionCode names what was built" >&2
      return 1
    fi
    export TETHER_ANDROID_AAB=1
    export TETHER_ANDROID_VERSION_CODE="${TETHER_BUILD_NUMBER:-$(git -C "$ROOT" rev-list --count HEAD)}"
    # Checked here: it is pasted into devvm.sh's remote command line.
    [[ "$TETHER_ANDROID_VERSION_CODE" =~ ^[1-9][0-9]*$ ]] \
      || { echo "build number must be a positive integer, got '$TETHER_ANDROID_VERSION_CODE'" >&2; return 1; }
    git -C "$ROOT" merge-base --is-ancestor HEAD main 2>/dev/null \
      || echo "warning: HEAD is not on main; another unmerged branch can get the same versionCode $TETHER_ANDROID_VERSION_CODE" >&2
    echo "versionCode $TETHER_ANDROID_VERSION_CODE ($(git -C "$ROOT" rev-parse --short HEAD))"
  fi

  # Signed results of an earlier build must not survive a failed one for `apk` / `aab` to hand over.
  [[ "$kind" == release ]] && ssh -n "${WIN_OPTS[@]}" "$WIN_HOST" 'del /q "%USERPROFILE%\termlyx-release.apk" 2>nul & exit /b 0'
  [[ "$kind" == play ]] && ssh -n "${WIN_OPTS[@]}" "$WIN_HOST" 'del /q "%USERPROFILE%\termlyx-play.aab" "%USERPROFILE%\termlyx-play-unsigned.aab" 2>nul & exit /b 0'

  "$ROOT/scripts/devvm.sh" sync
  local rc=0
  # devvm.sh run forwards TETHER_ANDROID_RELEASE to acceptance.sh.
  if [[ "$kind" == release ]]; then export TETHER_ANDROID_RELEASE=1; else unset TETHER_ANDROID_RELEASE; fi
  if [[ "$kind" == device ]]; then export TETHER_ANDROID_TARGET=aarch64; else unset TETHER_ANDROID_TARGET; fi
  "$ROOT/scripts/devvm.sh" run android-apk 2>&1 | tee "$TMP/build.out" || rc=$?
  ((rc == 0)) || { echo "android-apk failed on devvm (exit $rc)" >&2; return 1; }

  local apk tag=APK
  [[ "$kind" == play ]] && tag=AAB
  apk="$(sed -n "s/^$tag: //p" "$TMP/build.out" | tail -n 1 | tr -d '\r')"
  [[ -n "$apk" ]] || { echo "no '$tag:' line in the build output" >&2; return 1; }
  echo "$tag on devvm: $apk"

  scp "${DEVVM_OPTS[@]}" -q "$DEVVM_HOST:$apk" "$TMP/app.apk"
  # A relative scp path on win-dev lands in the user's home (C:\Users\<user>).
  scp "${WIN_OPTS[@]}" -q "$TMP/app.apk" "$WIN_HOST:$win_name"
  echo "copied to $WIN_HOST: ~\\$win_name ($(du -h "$TMP/app.apk" | cut -f1))"
  return 0
}

# fixture: devvm's Docker fixture -> the devvm tunnel (Mac localhost:2222) ->
# ssh -R (win-dev loopback :2222) -> the emulator's 10.0.2.2:2222.
# Stops what `fixture up` started, going by the markers: the forward (if its
# socket is up), the container (if owned), the devvm tunnel (if it opened it).
fixture_teardown() {
  ssh "${WIN_OPTS[@]}" -S "$CTL" -O exit "$WIN_HOST" >/dev/null 2>&1 || true
  if [[ -f "$OWNS_FIXTURE" ]]; then
    # down.sh needs devvm's Docker through the tunnel: reopen it if it died, and close it again then.
    local env_line
    "$ROOT/scripts/devvm.sh" status 2>/dev/null || : >"$OPENED_TUNNEL"
    if env_line="$("$ROOT/scripts/devvm.sh" up)"; then
      eval "$env_line"
      "$ROOT/tests/fixtures/down.sh" >/dev/null 2>&1 || echo "warning: tests/fixtures/down.sh failed" >&2
    else
      echo "warning: devvm tunnel would not open; tether-sshd may still run on devvm" >&2
    fi
  fi
  if [[ -f "$OPENED_TUNNEL" ]]; then
    "$ROOT/scripts/devvm.sh" down || true
  fi
  rm -f "$OWNS_FIXTURE" "$OPENED_TUNNEL"
}

# Opens the tunnel, starts the container (unless `running` is non-empty: ours
# from an earlier up) and the forward. Explicit returns: it runs under `||`, where set -e is off.
fixture_start() {
  local running=$1 env_line
  if ! "$ROOT/scripts/devvm.sh" status 2>/dev/null; then : >"$OPENED_TUNNEL"; fi
  env_line="$("$ROOT/scripts/devvm.sh" up)" || return 1
  eval "$env_line"
  if [[ -z "$running" ]]; then
    : >"$OWNS_FIXTURE"
    "$ROOT/tests/fixtures/up.sh" || return 1
  fi
  rm -f "$CTL"
  # -M -f -N: a background control master holding the forward until `down`;
  # fails now if win-dev's 2222 is taken.
  ssh "${WIN_OPTS[@]}" -S "$CTL" -M -f -N -o ExitOnForwardFailure=yes \
    -R 127.0.0.1:2222:127.0.0.1:2222 "$WIN_HOST" || return 1
}

fixture() {
  case "${1:-}" in
    up)
      if ssh "${WIN_OPTS[@]}" -S "$CTL" -O check "$WIN_HOST" >/dev/null 2>&1; then
        echo "fixture already up"
        return 0
      fi
      local running
      running="$(ssh -n "${DEVVM_OPTS[@]}" "$DEVVM_HOST" "docker ps -q --filter name=^tether-sshd\$")"
      mkdir -p "$STATE_DIR"
      chmod 700 "$STATE_DIR"
      if [[ -n "$running" && ! -f "$OWNS_FIXTURE" ]]; then
        # The container and the devvm tunnel are shared with acceptance.sh and win.sh.
        echo "tether-sshd is already running on devvm (acceptance.sh or win.sh run fixture?); not starting over it" >&2
        return 1
      fi
      # A stale marker (container gone) or a dead forward: start over from a clean state.
      [[ -z "$running" ]] && fixture_teardown
      # Any failure from here on undoes what this call started.
      fixture_start "$running" || { fixture_teardown; return 1; }
      echo "fixture up: the emulator reaches it at 10.0.2.2:2222 (user tether, key tests/fixtures/keys/id_ed25519_test). Stop it with: scripts/android.sh fixture down"
      ;;
    down)
      if [[ ! -f "$OWNS_FIXTURE" && ! -f "$OPENED_TUNNEL" ]] \
        && ! ssh "${WIN_OPTS[@]}" -S "$CTL" -O check "$WIN_HOST" >/dev/null 2>&1; then
        echo "fixture was not started by android.sh; nothing to stop" >&2
        return 0
      fi
      fixture_teardown
      echo "fixture down"
      ;;
    *) usage ;;
  esac
}

# Runs a command with a hard limit in seconds (macOS has no timeout(1)).
# Output goes to stdout; returns 124 on timeout.
with_timeout() {
  local secs=$1 pid watchdog rc=0
  shift
  "$@" &
  pid=$!
  (sleep "$secs"; kill "$pid" 2>/dev/null) &
  watchdog=$!
  wait "$pid" 2>/dev/null || rc=$?
  kill "$watchdog" 2>/dev/null || true
  wait "$watchdog" 2>/dev/null || true
  ((rc == 143)) && rc=124
  return "$rc"
}

# adb_ps <name> <powershell body using $adb>: sends the body as a .ps1 inside the
# Start-Job + 20 s timeout wrapper (no inline quoting over ssh), runs it, and
# prints its output. On failure or timeout prints the script for the owner.
adb_ps() {
  local name=$1 body=$2 ps="$TMP/tether-$1.ps1" rc=0 out
  cat >"$ps" <<EOF
\$adb = "\$env:LOCALAPPDATA\\Android\\Sdk\\platform-tools\\adb.exe"
\$j = Start-Job { param(\$adb)
  \$env:ANDROID_SERIAL = '${ANDROID_SERIAL:-}'
$body
} -ArgumentList \$adb
if (Wait-Job \$j -Timeout 20) { Receive-Job \$j 2>&1 | Out-String } else { 'adb timeout'; Stop-Job \$j }
EOF
  scp "${WIN_OPTS[@]}" -q "$ps" "$WIN_HOST:tether-$name.ps1" || rc=$?
  if ((rc == 0)); then
    out="$TMP/$name.out"
    with_timeout 45 ssh -n "${WIN_OPTS[@]}" "$WIN_HOST" \
      "powershell -NoProfile -ExecutionPolicy Bypass -File %USERPROFILE%\\tether-$name.ps1" >"$out" 2>&1 || rc=$?
    sed $'s/\r$//' "$out"
    grep -q '^adb timeout' "$out" && rc=124
  fi
  if ((rc != 0)); then
    echo "--- adb did not answer over SSH (exit $rc). Run this in PowerShell over RDP: ---" >&2
    cat "$ps" >&2
    return "$rc"
  fi
}

logcat() {
  local lines="${1:-300}"
  [[ "$lines" =~ ^[0-9]+$ ]] || usage
  adb_ps logcat "  & \$adb logcat -d -v time -t $lines"
}

shot() {
  local dest="${1:-${TMPDIR:-/tmp}/tether-shot.png}"
  # screencap on the emulator, then pull it into the user's home on win-dev.
  adb_ps shot "  & \$adb shell screencap -p /sdcard/tether-shot.png
  & \$adb pull /sdcard/tether-shot.png \"\$env:USERPROFILE\\tether-shot.png\" 2>&1 | Out-Null" || return 1
  scp "${WIN_OPTS[@]}" -q "$WIN_HOST:tether-shot.png" "$dest"
  echo "screenshot: $dest"
}

# The owner signs ~\termlyx-release-unsigned.apk over RDP (android-sign.ps1); this only fetches the result.
apk() {
  local dir="${1:-$ROOT/target/android}"
  mkdir -p "$dir"
  scp -p "${WIN_OPTS[@]}" -q "$WIN_HOST:termlyx-release.apk" "$dir/termlyx-release.apk" 2>/dev/null \
    || { echo "no ~\\termlyx-release.apk on win-dev: build release, then run scripts/win/android-sign.ps1 over RDP (scripts/win/README.md)" >&2; return 1; }
  echo "signed APK: $dir/termlyx-release.apk ($(du -h "$dir/termlyx-release.apk" | cut -f1), signed $(date -r "$dir/termlyx-release.apk" '+%F %T'); check it is from this build)"
}

# The owner signs ~\termlyx-play-unsigned.aab over RDP (android-sign.ps1 -Aab); this only fetches it.
aab() {
  local dir="${1:-$ROOT/target/android}"
  mkdir -p "$dir"
  scp -p "${WIN_OPTS[@]}" -q "$WIN_HOST:termlyx-play.aab" "$dir/termlyx-play.aab" 2>/dev/null \
    || { echo "no ~\\termlyx-play.aab on win-dev: build play, then run scripts/win/android-sign.ps1 -Aab over RDP (docs/store-release.md)" >&2; return 1; }
  echo "signed AAB: $dir/termlyx-play.aab ($(du -h "$dir/termlyx-play.aab" | cut -f1), signed $(date -r "$dir/termlyx-play.aab" '+%F %T'); check it is from this build)"
}

case "${1:-}" in
  build) shift; build "$@" ;;
  apk) shift; apk "$@" ;;
  aab) shift; aab "$@" ;;
  fixture) shift; fixture "$@" ;;
  logcat) shift; logcat "$@" ;;
  shot) shift; shot "$@" ;;
  *) usage ;;
esac
