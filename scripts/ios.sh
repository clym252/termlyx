#!/usr/bin/env bash
# iOS build and run, on demand (not part of acceptance.sh). See docs/ios-dev.md.
#
# Usage:
#   scripts/ios.sh sim-build            debug build for the Apple silicon simulator
#   scripts/ios.sh sim-run <device>     boot the simulator, install the last sim-build, launch
#                                       (e.g. "iPhone 17 Pro", "iPad Air 11-inch (M4)")
#   scripts/ios.sh device-run [device]  build, install and launch on a connected iPhone/iPad
#                                       (name, identifier or UDID from `xcrun devicectl list
#                                       devices`; optional when only one is connected)
#   scripts/ios.sh release [--upload]   store build for TestFlight: release archive, App Store
#                                       Connect export, checked, copied to target/ios/; --upload
#                                       also sends it with Xcode's signed-in account
#                                       (docs/store-release.md). Needs a clean work tree.
#   scripts/ios.sh clean                delete the Xcode build output (gen/apple/build, DerivedData)
#
# Env:
#   CARGO_TARGET_DIR          shared Rust build cache (recommended, see CLAUDE.md)
#   CARGO_PROFILE_DEV_DEBUG   Rust debug info for these builds (default 0: no debug
#                             info, which saves gigabytes of disk; 2 for full)
#   TETHER_BUILD_NUMBER       release: build number (default: git rev-list --count HEAD)
#
# Signing uses APPLE_DEVELOPMENT_TEAM, else the Team ID in ~/.config/termlyx/apple-team-id.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$ROOT/scripts/local-config.sh"
APP_DIR="$ROOT/apps/tauri"
APPLE_DIR="$APP_DIR/src-tauri/gen/apple"
BUNDLE_ID="ca.clym.termlyx"

usage() {
  sed -n '2,23p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
# Incremental caches are large and these builds are occasional.
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

# xcodegen fails on a missing source folder; Externals (built libs) is ignored by git.
mkdir -p "$APPLE_DIR/Externals"

# Empty (not a failure) before the first build, so callers can say so instead of exiting silently under `set -e`.
sim_app() {
  [[ -d "$APPLE_DIR/build" ]] || return 0
  find "$APPLE_DIR/build" -path '*arm64-sim*' -name 'Termlyx.app' -type d -print -quit 2>/dev/null || true
}

sim_build() {
  pnpm -C "$APP_DIR" tauri ios build --target aarch64-sim --debug --ci
  local app
  app="$(sim_app)"
  [[ -n "$app" ]] || { echo "built, but no Termlyx.app under $APPLE_DIR/build" >&2; exit 1; }
  echo "simulator app: $app"
}

sim_run() {
  local device="${1:?device name, e.g. \"iPhone 17 Pro\"}" app
  app="$(sim_app)"
  [[ -n "$app" ]] || { echo "no simulator build: run scripts/ios.sh sim-build first" >&2; exit 1; }
  # Already booted is fine.
  xcrun simctl boot "$device" 2>/dev/null || true
  xcrun simctl bootstatus "$device" -b >/dev/null
  open -a Simulator
  xcrun simctl install "$device" "$app"
  xcrun simctl launch "$device" "$BUNDLE_ID"
}

# The development team, also exported so the Tauri CLI signs with it (not stored in the repo).
dev_team() {
  if [[ -z "${APPLE_DEVELOPMENT_TEAM:-}" ]]; then
    local f="${XDG_CONFIG_HOME:-$HOME/.config}/termlyx/apple-team-id"
    [[ -s "$f" ]] || { echo "Set APPLE_DEVELOPMENT_TEAM or put your Apple Team ID in $f" >&2; exit 1; }
    APPLE_DEVELOPMENT_TEAM="$(tr -d '[:space:]' <"$f")"
  fi
  export APPLE_DEVELOPMENT_TEAM
  echo "$APPLE_DEVELOPMENT_TEAM"
}

# The Tauri CLI writes the Team ID into the committed project; delete just those lines on exit
# (success or failure) so it never gets committed. Other edits to the file are left alone.
drop_team_from_project() { sed -i '' '/DEVELOPMENT_TEAM = /d' "$APPLE_DIR/tether-app.xcodeproj/project.pbxproj" 2>/dev/null || true; }
trap drop_team_from_project EXIT

# Prints "identifier<TAB>udid<TAB>name<TAB>developer mode<TAB>pairing" for the device asked for
# (name, identifier or UDID; curly and straight apostrophes match), or the only connected
# iPhone/iPad when none is named. Explains and fails otherwise.
resolve_device() {
  local want="$1" json out
  json="$(mktemp -t tether-devices)"
  if ! out="$(xcrun devicectl list devices --json-output "$json" 2>&1)"; then
    rm -f "$json"
    echo "xcrun devicectl list devices failed:" >&2
    tail -n 15 <<<"$out" >&2
    return 1
  fi
  python3 -I - "$json" "$want" <<'PY'
import json, sys

path, want = sys.argv[1], sys.argv[2]
devices = json.load(open(path))["result"]["devices"]

def norm(s):
    return (s or "").replace("\u2019", "'").replace("\u2018", "'").strip().casefold()

rows = []
for d in devices:
    props, hw, conn = d.get("deviceProperties", {}), d.get("hardwareProperties", {}), d.get("connectionProperties", {})
    if hw.get("platform") not in ("iOS", "iPadOS"):
        continue
    rows.append({
        "id": d.get("identifier", ""),
        "udid": hw.get("udid", ""),
        "name": props.get("name", ""),
        "devmode": props.get("developerModeStatus", "unknown"),
        "pairing": conn.get("pairingState", "unknown"),
        "connected": bool(conn.get("transportType")) and conn.get("tunnelState") != "unavailable",
    })

def show(rs):
    return "\n".join(f"  {r['name']}  ({r['id']}, {'connected' if r['connected'] else 'not connected'})" for r in rs) or "  (none)"

if want:
    w = norm(want)
    hits = [r for r in rows if w in (norm(r["name"]), norm(r["id"]), norm(r["udid"]))]
    if not hits:
        sys.exit(f"No device named or identified \"{want}\". Devices Xcode knows:\n{show(rows)}")
    if len(hits) > 1:
        sys.exit(f"More than one device matches \"{want}\"; use its identifier:\n{show(hits)}")
else:
    hits = [r for r in rows if r["connected"]]
    if not hits:
        sys.exit("No iPhone or iPad is connected. Connect it with a cable (unlocked, Trust tapped), then retry.\n"
                 f"Devices Xcode knows:\n{show(rows)}")
    if len(hits) > 1:
        sys.exit(f"More than one device is connected; name one:\n{show(hits)}")
r = hits[0]
print("\t".join([r["id"], r["udid"], r["name"], r["devmode"], r["pairing"]]))
PY
  local status=$?
  rm -f "$json"
  return $status
}

# Whether an installed, unexpired provisioning profile for the app lists this device.
has_profile() {
  local udid="$1" team="$2"
  python3 -I - "$udid" "$team.$BUNDLE_ID" "$team" <<'PY'
import datetime, glob, os, plistlib, subprocess, sys

udid, app_id, team = sys.argv[1:4]
dirs = ["~/Library/Developer/Xcode/UserData/Provisioning Profiles", "~/Library/MobileDevice/Provisioning Profiles"]
now = datetime.datetime.now(datetime.timezone.utc)
for d in dirs:
    for f in glob.glob(os.path.join(os.path.expanduser(d), "*.mobileprovision")):
        out = subprocess.run(["security", "cms", "-D", "-i", f], capture_output=True).stdout
        try:
            p = plistlib.loads(out)
        except Exception:
            continue
        ident = p.get("Entitlements", {}).get("application-identifier", "")
        expires = p.get("ExpirationDate")
        if expires is not None and expires.tzinfo is None:
            expires = expires.replace(tzinfo=datetime.timezone.utc)
        if ident in (app_id, f"{team}.*") and udid in p.get("ProvisionedDevices", []) and (expires is None or expires > now):
            sys.exit(0)
sys.exit(1)
PY
}

# A device the team has never seen: `tauri ios build` archives for a generic iOS destination, so
# Xcode has no device to register and fails with "Your team has no devices from which to generate
# a provisioning profile". A one-off build for this device lets Xcode register it and make the
# profile. Only signing matters here: the Rust phase (which needs a running Tauri CLI) is skipped
# with a no-op `pnpm`, and the build's own result is ignored.
register_device() {
  local udid="$1" log
  tmp="$(mktemp -d -t tether-register)"
  # Global (not local) so the trap still sees it when the function has returned or exited.
  trap 'rm -rf "$tmp"; drop_team_from_project' EXIT INT TERM
  log="$tmp/xcodebuild.log"
  printf '#!/bin/sh\nexit 0\n' >"$tmp/pnpm"
  chmod +x "$tmp/pnpm"
  echo "First install on this device: registering it with your team (one time)..."
  PATH="$tmp:$PATH" xcodebuild \
    -project "$APPLE_DIR/tether-app.xcodeproj" -scheme tether-app_iOS -configuration debug \
    -destination "id=$udid" -derivedDataPath "$tmp/DerivedData" \
    -allowProvisioningUpdates -allowProvisioningDeviceRegistration \
    build >"$log" 2>&1 || true
  if ! has_profile "$udid" "$TEAM"; then
    echo "Could not register the device with team $TEAM. Xcode said:" >&2
    grep -E "error:|warning: .*(provision|sign)" "$log" | head -n 8 >&2 || tail -n 20 "$log" >&2
    if grep -qi "developer mode disabled" "$log"; then
      explain_developer_mode
    else
      echo "Check Xcode → Settings… → Accounts lists your Apple ID (team $TEAM), then retry." >&2
    fi
    exit 1
  fi
  rm -rf "$tmp"
  trap drop_team_from_project EXIT; trap - INT TERM
  echo "Registered."
}

# devicectl's output for a failed install or launch (step: install|launch), explained when the
# cause is a known one.
explain_device_error() {
  local out="$1" step="$2"
  if grep -qiE "not been explicitly trusted|untrusted|invalid code signature|profile.*(not|un)trusted|Security policy" <<<"$out"; then
    cat >&2 <<'MSG'
iOS does not trust this developer profile yet (first install from this Mac's team).
On the device: Settings → General → VPN & Device Management → under "Developer App" tap your
Apple ID → Trust "…" → Trust. Then open Termlyx from the Home Screen (or rerun device-run).
MSG
  elif grep -qiE "developer mode" <<<"$out"; then
    explain_developer_mode
  elif grep -qiE "(^|[^[:alpha:]])locked([^[:alpha:]]|$)" <<<"$out"; then
    if [[ "$step" == "launch" ]]; then
      echo "Termlyx is installed, but the device is locked: unlock it and open Termlyx from the Home Screen." >&2
    else
      echo "The device is locked: unlock it and rerun device-run." >&2
    fi
  fi
}

explain_developer_mode() {
  cat >&2 <<'MSG'
Developer Mode is off on the device. On the device: Settings → Privacy & Security → scroll to the
bottom → Developer Mode → on → Restart; after the restart tap Turn On and enter the passcode.
Then rerun device-run.
MSG
}

device_run() {
  local info id udid name devmode pairing app out
  info="$(resolve_device "${1:-}")" || exit 1
  IFS=$'\t' read -r id udid name devmode pairing <<<"$info"
  echo "Device: $name ($id)"
  if [[ "$devmode" == "disabled" ]]; then
    explain_developer_mode
    exit 1
  fi
  if [[ "$pairing" != "paired" ]]; then
    echo "The Mac is not paired with $name: connect it with a cable, unlock it, tap Trust and enter the passcode, then retry." >&2
    exit 1
  fi
  TEAM="$(dev_team)"
  has_profile "$udid" "$TEAM" || register_device "$udid"

  # Debug build with the bundled frontend (no dev server). Archive only: the IPA export step is
  # not needed to install, and has failed after a good build ("Couldn't load -exportOptionsPlist").
  pnpm -C "$APP_DIR" tauri ios build --debug --target aarch64 --ci --archive-only
  app="$APPLE_DIR/build/tether-app_iOS.xcarchive/Products/Applications/Termlyx.app"
  [[ -d "$app" ]] || { echo "built, but no Termlyx.app in the archive ($app)" >&2; exit 1; }

  echo "Installing on $name..."
  if ! out="$(xcrun devicectl device install app --device "$id" "$app" 2>&1)"; then
    echo "$out" >&2
    explain_device_error "$out" install
    exit 1
  fi
  echo "Launching..."
  if ! out="$(xcrun devicectl device process launch --device "$id" --terminate-existing "$BUNDLE_ID" 2>&1)"; then
    echo "$out" >&2
    explain_device_error "$out" launch
    exit 1
  fi
  echo "Termlyx is running on $name."
}

# Store build for TestFlight (docs/store-release.md). Release archive with build number N
# (CFBundleVersion), exported for App Store Connect with Xcode's signed-in account and
# cloud-managed signing (no API key, no local distribution certificate), checked, copied to
# target/ios/. --upload then sends it to App Store Connect the same way.
release() {
  local upload=0
  case "${1:-}" in
    "") ;;
    --upload) upload=1 ;;
    *) usage ;;
  esac
  if [[ -n "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ]]; then
    echo "uncommitted changes: commit first, so the build number names what was built" >&2
    git -C "$ROOT" status --short --untracked-files=no >&2
    exit 1
  fi
  local n="${TETHER_BUILD_NUMBER:-$(git -C "$ROOT" rev-list --count HEAD)}" version
  [[ "$n" =~ ^[1-9][0-9]*$ ]] || { echo "build number must be a positive integer, got '$n'" >&2; exit 1; }
  git -C "$ROOT" merge-base --is-ancestor HEAD main 2>/dev/null \
    || echo "warning: HEAD is not on main; another unmerged branch can get the same build number $n" >&2
  version="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$APP_DIR/src-tauri/tauri.conf.json")"
  echo "Termlyx $version build $n ($(git -C "$ROOT" rev-parse --short HEAD))"

  rtmp="$(mktemp -d -t tether-release)"
  # The build rewrites committed files under gen/apple (Info.plist version, ExportOptions.plist);
  # the tree was clean, so restoring them drops only the build's edits.
  trap 'git -C "$ROOT" checkout -- "$APPLE_DIR"; rm -rf "$rtmp"' EXIT INT TERM
  pnpm -C "$APP_DIR" tauri ios build --target aarch64 --ci --archive-only \
    --config "{\"bundle\":{\"iOS\":{\"bundleVersion\":\"$n\"}}}"
  local archive="$APPLE_DIR/build/tether-app_iOS.xcarchive"
  [[ -d "$archive" ]] || { echo "built, but no archive at $archive" >&2; exit 1; }

  export_archive export "$archive" "$rtmp/export"
  local ipa="$rtmp/export/Termlyx.ipa"
  [[ -f "$ipa" ]] || { echo "export succeeded, but no Termlyx.ipa in it:" >&2; ls "$rtmp/export" >&2; exit 1; }
  check_ipa "$ipa" "$n"
  mkdir -p "$ROOT/target/ios"
  local out="$ROOT/target/ios/Termlyx-$version-$n.ipa"
  cp "$ipa" "$out"
  echo "IPA: $out"

  if ((upload)); then
    echo "Uploading build $n to App Store Connect..."
    export_archive upload "$archive" "$rtmp/upload"
    echo "Uploaded. App Store Connect → Termlyx → TestFlight shows build $n after processing (a few minutes to ~30)."
  else
    echo "Not uploaded: rerun with --upload, or drag the IPA into Transporter."
  fi
}

# export_archive export|upload <archive> <dir>: xcodebuild -exportArchive for App Store Connect.
export_archive() {
  local destination=$1 archive=$2 dir=$3 plist="$rtmp/ExportOptions-$1.plist" log="$rtmp/export-$1.log"
  cat >"$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key><string>app-store-connect</string>
  <key>destination</key><string>$destination</string>
  <key>signingStyle</key><string>automatic</string>
  <key>teamID</key><string>$(dev_team)</string>
  <key>manageAppVersionAndBuildNumber</key><false/>
  <key>uploadSymbols</key><true/>
</dict>
</plist>
EOF
  if ! xcodebuild -exportArchive -archivePath "$archive" -exportPath "$dir" \
    -exportOptionsPlist "$plist" -allowProvisioningUpdates >"$log" 2>&1; then
    echo "xcodebuild -exportArchive ($destination) failed:" >&2
    grep -E "error|Error" "$log" | head -n 15 >&2 || tail -n 25 "$log" >&2
    if grep -qiE "no accounts|account.*(not|expired)|sign in|authenticat" "$log"; then
      echo "Xcode → Settings… → Accounts must list your Apple ID with team $(dev_team), signed in." >&2
    fi
    exit 1
  fi
}

# check_ipa <ipa> <build number>: the store build has the right number, the encryption
# declaration and an Apple Distribution signature.
check_ipa() {
  local ipa=$1 n=$2 d="$rtmp/check" info got enc auth
  unzip -q "$ipa" -d "$d"
  info="$d/Payload/Termlyx.app/Info.plist"
  got="$(plutil -extract CFBundleVersion raw "$info")"
  enc="$(plutil -extract ITSAppUsesNonExemptEncryption raw "$info" 2>/dev/null || echo missing)"
  auth="$(codesign -dvv "$d/Payload/Termlyx.app" 2>&1 | sed -n 's/^Authority=//p' | head -n 1)"
  echo "IPA check: CFBundleVersion $got, ITSAppUsesNonExemptEncryption $enc, signed by \"$auth\""
  [[ "$got" == "$n" ]] || { echo "CFBundleVersion is $got, expected $n" >&2; exit 1; }
  [[ "$enc" == false ]] || { echo "ITSAppUsesNonExemptEncryption is $enc, expected false" >&2; exit 1; }
  [[ "$auth" == "Apple Distribution"* ]] || { echo "not signed for distribution" >&2; exit 1; }
}

clean() {
  rm -rf "$APPLE_DIR/build"
  rm -rf "$HOME"/Library/Developer/Xcode/DerivedData/tether-app-*
  echo "removed $APPLE_DIR/build and Xcode DerivedData for tether-app"
}

[[ "${1:-}" == clean || -z "${1:-}" ]] || dev_team >/dev/null
# rust-toolchain.toml pins an exact version, which starts without the iOS targets
# (run from the repo so rustup reads that file).
ios_targets() { (cd "$ROOT" && rustup target add aarch64-apple-ios aarch64-apple-ios-sim); }

case "${1:-}" in
  sim-build) ios_targets; sim_build ;;
  sim-run) shift; sim_run "$@" ;;
  device-run) ios_targets; shift; device_run "${1:-}" ;;
  release) ios_targets; shift; release "$@" ;;
  clean) clean ;;
  *) usage ;;
esac
