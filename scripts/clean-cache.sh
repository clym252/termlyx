#!/usr/bin/env bash
# Reclaim disk from Tether builds. Third-party crates stay compiled.
#
# Usage:
#   scripts/clean-cache.sh [--dry-run]        clean the Mac build cache
#   scripts/clean-cache.sh --vm [--dry-run]   clean the Windows VM's C:\src\termlyx\target instead,
#                                             then devvm's Android build output
#
# Removes, and nothing else:
#   - Tether's own crates (workspace packages) from $CARGO_TARGET_DIR, for
#     debug, release, aarch64-apple-ios-sim and aarch64-apple-ios (cargo clean -p)
#   - */incremental under $CARGO_TARGET_DIR
#   - this checkout's apps/tauri/src-tauri/gen/apple/build and Xcode
#     DerivedData/tether-app-* (the same paths as scripts/ios.sh clean)
# --vm: the same crates (debug, release) and incremental dirs on the VM; then on
#   devvm (where Android builds): gen/android/**/build under ~/src/termlyx and the
#   *-linux-android dirs of its cargo target dir (third-party deps included: they
#   rebuild in the next `scripts/android.sh build`).
# --dry-run prints what would go, with sizes. Free space is printed before and after.
#
# Env:
#   CARGO_TARGET_DIR  shared build cache (required)
#   TETHER_WIN_HOST, TETHER_WIN_KEY  as for scripts/win.sh
#   TETHER_DEVVM_HOST, TETHER_DEVVM_KEY  as for scripts/devvm.sh (--vm)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ROOT/scripts/local-config.sh"
TARGET="${CARGO_TARGET_DIR:?set CARGO_TARGET_DIR (shared build cache)}"
APPLE_BUILD="$ROOT/apps/tauri/src-tauri/gen/apple/build"
DERIVED="$HOME/Library/Developer/Xcode/DerivedData"
HOST="${TETHER_WIN_HOST:?set TETHER_WIN_HOST (user@host of the Windows VM)}"
KEY="${TETHER_WIN_KEY:-$HOME/.ssh/id_ed25519}"
SSH_OPTS=(-i "$KEY" -o IdentitiesOnly=yes -o BatchMode=yes -o ConnectTimeout=15)
DEVVM_HOST="${TETHER_DEVVM_HOST:?set TETHER_DEVVM_HOST (user@host of the Linux build VM)}"
DEVVM_KEY="${TETHER_DEVVM_KEY:-$HOME/.ssh/id_ed25519}"

usage() {
  sed -n '2,24p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

DRY=0
VM=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY=1 ;;
    --vm) VM=1 ;;
    *) usage ;;
  esac
done

packages() {
  cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT/Cargo.toml" |
    python3 -c 'import json, sys; print(" ".join(p["name"] for p in json.load(sys.stdin)["packages"]))'
}

# Removes a path (or prints its size under --dry-run).
remove() {
  local path=$1
  if ((DRY)); then
    printf '  would remove %s  %s\n' "$(du -sh "$path" 2>/dev/null | cut -f1)" "$path"
  else
    rm -rf "$path"
    echo "  removed $path"
  fi
}

clean_mac() {
  [[ -d "$TARGET" ]] || { echo "no target dir at $TARGET (set CARGO_TARGET_DIR)" >&2; exit 1; }
  # Guard against pointing at a non-cargo dir: rm -rf runs inside it.
  [[ -f "$TARGET/CACHEDIR.TAG" ]] || { echo "$TARGET is not a cargo target dir (no CACHEDIR.TAG)" >&2; exit 1; }

  echo "free before: $(df -h / | awk 'NR==2 {print $4}')"
  local pkgs p_args=() pkg dry_arg=() flags
  pkgs="$(packages)"
  for pkg in $pkgs; do p_args+=(-p "$pkg"); done
  ((DRY)) && dry_arg=(--dry-run)

  echo "Tether crates in $TARGET: $pkgs"
  # Profile/target flags, paired with the dir each one cleans.
  while read -r dir flags; do
    [[ -d "$TARGET/$dir" ]] || continue
    echo "  $dir:"
    # shellcheck disable=SC2086 # flags is a word list on purpose
    cargo clean --manifest-path "$ROOT/Cargo.toml" --target-dir "$TARGET" $flags "${p_args[@]}" ${dry_arg[@]+"${dry_arg[@]}"} 2>&1 |
      grep -E '^ *(Removed|Summary)' | sed 's/^ */    /' || true
  done <<'EOF'
debug
release --release
aarch64-apple-ios-sim/debug --target aarch64-apple-ios-sim
aarch64-apple-ios-sim/release --target aarch64-apple-ios-sim --release
aarch64-apple-ios/debug --target aarch64-apple-ios
aarch64-apple-ios/release --target aarch64-apple-ios --release
EOF

  echo "incremental dirs:"
  local dir
  while IFS= read -r dir; do remove "$dir"; done < <(find "$TARGET" -maxdepth 3 -type d -name incremental -prune)

  echo "Xcode build output:"
  [[ -d "$APPLE_BUILD" ]] && remove "$APPLE_BUILD"
  for dir in "$DERIVED"/tether-app-*; do
    [[ -d "$dir" ]] && remove "$dir"
  done

  echo "free after:  $(df -h / | awk 'NR==2 {print $4}')"
}

# Same crates and incremental dirs on the VM, as one PowerShell script (the VM's
# SSH shell is cmd, so it goes base64-encoded to dodge cmd quoting).
clean_vm() {
  local pkgs list ps
  read -r -a pkgs <<<"$(packages)"
  list="$(printf "'%s'," "${pkgs[@]}")"
  ps=$(cat <<EOF
\$ErrorActionPreference = 'Stop'
\$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')
\$dry = $DRY
\$target = 'C:\src\termlyx\target'
function Free { '{0:N1} GB' -f ((Get-PSDrive C).Free / 1GB) }
if (-not (Test-Path (Join-Path \$target 'CACHEDIR.TAG'))) { Write-Host "no cargo target dir at \$target"; exit 1 }
Write-Host "free before: \$(Free)"
Set-Location 'C:\src\termlyx'
\$p = @(${list%,}) | ForEach-Object { '-p'; \$_ }
\$d = @(); if (\$dry) { \$d = @('--dry-run') }
Write-Host "Tether crates in \$target"
foreach (\$prof in @(@('debug'), @('release', '--release'))) {
  if (-not (Test-Path (Join-Path \$target \$prof[0]))) { continue }
  Write-Host "  \$(\$prof[0]):"
  \$flags = @(\$prof | Select-Object -Skip 1)
  cmd /c "cargo clean \$flags \$p \$d 2>&1" | Select-String -Pattern '^\s*(Removed|Summary)' | ForEach-Object { "    " + \$_.Line.Trim() }
}
Write-Host 'incremental dirs:'
foreach (\$dir in Get-ChildItem \$target -Directory | ForEach-Object { Join-Path \$_.FullName 'incremental' } | Where-Object { Test-Path \$_ }) {
  if (\$dry) {
    \$size = (Get-ChildItem \$dir -Recurse -File -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
    Write-Host ('  would remove {0:N1}G  {1}' -f (\$size / 1GB), \$dir)
  } else {
    Remove-Item -Recurse -Force \$dir
    Write-Host "  removed \$dir"
  }
}
Write-Host "free after:  \$(Free)"
EOF
)
  # -EncodedCommand writes its streams to stderr as CLIXML; show that only on
  # failure, as the error records' text (or as is, for an ssh error).
  local err rc=0
  err="$(mktemp)"
  # shellcheck disable=SC2029 # expanded on the Mac on purpose
  ssh -n "${SSH_OPTS[@]}" "$HOST" \
    powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand "$(printf '%s' "$ps" | iconv -t UTF-16LE | base64)" \
    2>"$err" | sed $'s/\r$//' || rc=$?
  if ((rc != 0)); then
    if grep -q '^#< CLIXML' "$err"; then
      grep -o '<S S="Error">[^<]*' "$err" | sed 's/^<S S="Error">//; s/_x000D_//g; s/_x000A_//g' >&2
    else
      cat "$err" >&2
    fi
  fi
  rm -f "$err"
  return "$rc"
}

# devvm builds the Android APKs: report/remove gen/android build output and the
# Android target dirs of its cargo target dir (default ~/src/termlyx/target).
clean_devvm() {
  echo "devvm Android build output:"
  # shellcheck disable=SC2029 # DRY expands on the Mac on purpose
  ssh -i "$DEVVM_KEY" -o IdentitiesOnly=yes -o BatchMode=yes -o ConnectTimeout=15 "$DEVVM_HOST" \
    "DRY=$DRY bash -s" <<'REMOTE'
free() { df -h / | awk 'NR==2 {print $4}'; }
echo "free before: $(free)"
target="${CARGO_TARGET_DIR:-$HOME/src/termlyx/target}"
found=0
for dir in "$HOME"/src/termlyx/apps/tauri/src-tauri/gen/android/build \
  "$HOME"/src/termlyx/apps/tauri/src-tauri/gen/android/*/build "$target"/*-linux-android; do
  [ -d "$dir" ] || continue
  found=1
  if [ "$DRY" = 1 ]; then
    echo "  would remove $(du -sh "$dir" | cut -f1)  $dir"
  else
    rm -rf "$dir"
    echo "  removed $dir"
  fi
done
[ "$found" = 1 ] || echo "  nothing to remove"
echo "free after:  $(free)"
REMOTE
}

if ((VM)); then clean_vm; clean_devvm; else clean_mac; fi
