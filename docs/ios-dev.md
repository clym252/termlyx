# iOS development

Termlyx builds for iOS 18 and later (iPhone and iPad) from the same Tauri
app. iOS steps run on demand with `scripts/ios.sh`; they are not part of
`scripts/acceptance.sh`.

## Simulator

```sh
export CARGO_TARGET_DIR=$HOME/tether-target
scripts/ios.sh sim-build                      # debug build for the Apple silicon simulator (~5 min cold)
scripts/ios.sh sim-run "iPhone 17 Pro"        # boots, installs, launches
scripts/ios.sh sim-run "iPad Air 11-inch (M4)"
scripts/ios.sh clean                          # deletes gen/apple/build and Xcode DerivedData for tether-app
```

The simulator shares the Mac's network: the SSH fixture (`tests/fixtures/up.sh`)
is at `127.0.0.1:2222`, user `tether`. iOS has no local login name, so a host
needs an identity with a username (Keychain → New identity, then the host's
Identity → Override).

`sim-build` builds Rust without debug info by default (it saves gigabytes);
set `CARGO_PROFILE_DEV_DEBUG=2` for full debug info.

The simulator starts with "Connect Hardware Keyboard" on, so the software
keyboard (and the terminal's key bar) does not show. To see them: Simulator →
I/O → Keyboard → Toggle Software Keyboard (⌘K).

## Terminal keyboard on iOS

- **Key bar** (`apps/tauri/src/terminal/KeyBar.tsx`, keys in `keyMap.ts`): above
  the software keyboard, shown only while one is up (the visual viewport lost
  more than 120 px, `viewport.ts`) and the terminal has the focus.
  `ctrl`/`alt`: tap once for the next key (bar or keyboard; the key turns
  tinted and outlined), twice to lock (filled, with a bar under the label), a
  third time to release. Arrows repeat while held.
  - **Size**: the shrink key (two chevrons closing in) steps two rows → one
    row (`esc ctrl alt tab ← ↑ ↓ →`) → hidden, where only a small **keys**
    pill ("Show key bar") stays on the keyboard's edge; the pill brings both
    rows back. Kept per device in localStorage (`tether.keyBar`,
    `keyBarPref.ts`). Hiding the bar releases a held ctrl/alt.
  - **Hide keyboard** (`⌄`, last key): the terminal lets go of the focus, so
    the keyboard and the bar go away; tap the terminal to bring them back.
- **Touch scrolling** (`apps/tauri/src/terminal/touchScroll.ts`): xterm 6
  scrolls with the wheel only (its viewport is a custom scrollable element
  with no touch handling), so the app scrolls itself on a one-finger vertical
  drag, with momentum after a flick. Normal screen: the scrollback. Alternate
  screen (vim, less, htop): one wheel step per line of finger travel, which
  xterm sends as mouse wheel reports when the app turned mouse reporting on,
  and as Up/Down arrow keys otherwise. There a flick carries on only with
  mouse reporting, for at most 25 steps; without it the drag alone counts
  (no burst of arrow keys after the finger lifts). A tap still focuses the terminal, a
  sideways swipe from the left edge still opens the drawer, a touch held
  still first (long press) is left alone, and a touch during a fling stops it.
- **No prev/next/Done bar**: `apps/tauri/src-tauri/src/ios.rs` replaces
  `-[WKContentView inputAccessoryView]` with one returning nil at startup
  (objc2; no Swift file, so the generated Xcode project is unchanged). If a
  WebKit update renames the class, the app logs a warning and the bar returns.
- **Applies to every text field, not just the terminal.** Both swaps are on
  `WKContentView`, the one class behind every web view, so forms (host editor,
  Settings, unlock screen) also lose prev/next/Done and, on an iPad, the
  undo/redo/paste strip. Accepted for Phase 4: the native side cannot tell which
  DOM element has the focus, the forms move between fields by tapping, and
  undo/redo/paste stay in the touch-and-hold menu. To scope it to the terminal
  later: have the web layer report focus in and out of xterm's
  `.xterm-helper-textarea` (a small Tauri command setting an atomic flag), keep
  the original IMPs when swapping, and return the original accessory view and
  shortcut groups while the flag is off (then `reloadInputViews` on the first
  responder so the change shows without a refocus).

## Your iPhone or iPad (free Apple account)

Signing uses your Apple development team. Put its 10-character Team ID in
`~/.config/termlyx/apple-team-id` (one line; or export `APPLE_DEVELOPMENT_TEAM`);
`scripts/ios.sh` reads it. It is not stored in the repository. A free account's install **expires
after 7 days**: Termlyx then will not open until you install it again with
`device-run` (the vault on the device is kept).

One-time setup on the Mac:

1. Open Xcode → Settings… → Accounts. If your Apple ID is not listed, click
   **+**, choose **Apple ID**, sign in.
2. Connect the iPhone or iPad with a cable. On the device, tap **Trust** and
   enter its passcode.

One-time setup on the device:

1. Settings → Privacy & Security → scroll to the bottom → **Developer Mode** →
   turn it on → **Restart**. After the restart, tap **Turn On** and enter the
   passcode.

Each install (and again after 7 days), from the worktree on the Mac, with
the device unlocked:

```sh
export CARGO_TARGET_DIR=$HOME/tether-target
scripts/ios.sh device-run                      # the only connected iPhone/iPad
scripts/ios.sh device-run "<device name>"      # or name one (name, identifier or UDID)
```

What `device-run` does:

1. Finds the device in `xcrun devicectl list devices` (a straight `'` matches
   the curly `’` iOS puts in device names). Stops with the steps to follow if
   Developer Mode is off or the Mac is not paired.
2. A device your team has never seen (no installed provisioning profile lists
   it, or the free account's profile expired): one short `xcodebuild` for that
   device with `-allowProvisioningUpdates -allowProvisioningDeviceRegistration`,
   which registers it and makes the profile. Without it, `tauri ios run/build`
   fail with "Your team has no devices from which to generate a provisioning
   profile", because they build for a generic iOS destination. The Rust phase
   is skipped in that build (a no-op `pnpm` on its PATH: the real phase needs a
   running Tauri CLI) and the build's own result is ignored; only signing is
   wanted from it.
3. `tauri ios build --debug --target aarch64 --ci --archive-only` (the
   bundled frontend, no dev server). No IPA export: it is not needed to
   install, and it has failed after a good build ("Couldn't load
   -exportOptionsPlist … no such file").
4. `xcrun devicectl device install app` with the archive's `Termlyx.app`, then
   `xcrun devicectl device process launch --terminate-existing
   ca.clym.termlyx`.

If it stops:

- **"Developer Mode is off"**: do the one-time device setup above.
- **"iOS does not trust this developer profile"** (first install from this
  Mac's team): on the device, Settings → General → **VPN & Device
  Management** → under Developer App, tap your Apple ID → **Trust "…"** →
  **Trust**. Then open Termlyx from the Home Screen.
- **"the device is locked"**: Termlyx is installed; unlock the device and open
  Termlyx from the Home Screen.
- **"Could not register the device"**: Xcode → Settings… → Accounts must list
  your Apple ID for the team; then run `device-run` again.

The first connection to a host on your network asks for Local Network access:
tap **Allow**. (If you tapped Don't Allow: Settings → Privacy & Security →
Local Network → turn on Termlyx.)

Do not open the project in Xcode to fix signing: accepting its "recommended
settings" turns the user-script sandbox back on, which breaks the Rust build
phase (`ENABLE_USER_SCRIPT_SANDBOXING` is pinned off in `project.yml`).

## What is stored where on iOS

- Vault: the app's data folder (as on the Mac).
- Quick unlock device key and sync tokens: the iOS keychain, as
  "this device only, while unlocked" items (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`):
  never in iCloud Keychain, never restored to another device from a backup.
  See `crates/core-api/src/vault/keychain.rs`.
