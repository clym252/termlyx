# Dev code signing (optional, macOS)

`pnpm -C apps/tauri tauri dev` rebuilds the app on every Rust change. macOS
Keychain approvals ("Always Allow") are tied to who signed the app, so an
unsigned dev build looks like a brand-new app after every rebuild and asks for
Keychain access again.

Signing the dev binary with your **Apple Development** certificate fixes this.
This is **opt-in**: nothing changes unless you set `TETHER_DEV_SIGN_IDENTITY`
(step 4).

**Termlyx's scripts never create, import or trust a certificate, and never
change any security setting.** You do the keychain steps below yourself. The
only thing the repo's script (`scripts/dev-sign-run.sh`) does is run `codesign`
with the identity you name, on the freshly built debug binary.

## Why it must be an Apple Development certificate

Each Keychain item keeps a *partition list* of apps allowed to read it. For a
binary signed with a **Team ID**, macOS records `teamid:<TEAM>`, which stays
the same across rebuilds. For a binary with **no** Team ID, macOS records the
binary's exact hash (`cdhash:…`), which changes on every rebuild.

A self-signed certificate (for example one made with Keychain Access's
Certificate Assistant) has no Team ID, so it does **not** stop the prompts. An
Apple Development certificate has one. Any Apple ID works: a free account in
Xcode gets a "Personal Team". You need this certificate for the iPhone build
anyway.

## 1. Get an Apple Development certificate

1. Open **Xcode** > **Settings…** > **Accounts**.
2. Click **+** and sign in with your Apple ID, or select it if it is already
   listed.
3. Select your team, click **Manage Certificates…**, then **+** >
   **Apple Development**. Close the window.

## 2. Check that macOS accepts it

```sh
security find-identity -v -p codesigning
```

You should see a line like
`1) 5812…0B13 "Apple Development: you@example.com (ABCDE12345)"`.

**If it is missing from the `-v` list** but `security find-identity -p
codesigning` (without `-v`) shows it, Apple's intermediate certificate
("Worldwide Developer Relations - G3") is probably missing. Install it into
your **login** keychain:

```sh
curl -fsSLo ~/Downloads/AppleWWDRCAG3.cer https://www.apple.com/certificateauthority/AppleWWDRCAG3.cer
security add-certificates -k ~/Library/Keychains/login.keychain-db ~/Downloads/AppleWWDRCAG3.cer
```

(Double-clicking the `.cer` can fail with "The System Roots keychain cannot be
modified" if Keychain Access has System Roots selected; the command above
avoids that.) An expired older "Apple Worldwide Developer Relations
Certification Authority" (2023) in your login keychain can be deleted.

Then run the `find-identity -v` command again.

## 3. Let `codesign` use the key without a prompt

The first signing otherwise shows "codesign wants to access key …", and that
dialog can get stuck with greyed-out buttons. Allow `codesign` to use this one
key (it asks for your login keychain password in the terminal; the
"(deprecated)" in its prompt is normal):

```sh
security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
  -l "Apple Development: Your Name (Your Name)" ~/Library/Keychains/login.keychain-db
```

Use the key's name as shown in that dialog or in Keychain Access > **login** >
**Keys**. On success it prints the key's attributes.

## 4. Turn it on

Add this to your shell profile (for example `~/.zshrc`) and open a new
terminal:

```sh
export TETHER_DEV_SIGN_IDENTITY="Apple Development: you@example.com (ABCDE12345)"
```

If `find-identity -v` lists **two** certificates with the same name (Xcode
created a new one), `codesign` refuses an ambiguous name: use the newer one's
40-character ID from that list instead, for example
`export TETHER_DEV_SIGN_IDENTITY="5812…0B13"` (the full ID).

Then run `pnpm -C apps/tauri tauri dev` as usual. Cargo runs
`scripts/dev-sign-run.sh` for the dev binary (configured in
`apps/tauri/src-tauri/.cargo/config.toml`), which signs it with
`codesign --force --sign "$TETHER_DEV_SIGN_IDENTITY" --identifier ca.clym.termlyx`
and then starts it. If signing fails, it prints one line,
`dev-sign: codesign failed; running unsigned (see docs/dev-signing.md)`, and
starts the app unsigned.

The first unlock after switching asks **one last time** for Termlyx's Keychain
items (click **Always Allow**). Later rebuilds should not ask again.

To confirm the build carries your Team ID:

```sh
codesign -dvv path/to/target/debug/tether-app 2>&1 | grep -E 'Authority=Apple|TeamIdentifier'
```

`TeamIdentifier` must be a team ID, not `not set`.

## Troubleshooting

To see why signing failed (the script hides `codesign`'s own output), run the
same command by hand on the dev binary, which is `debug/tether-app` under
cargo's target directory (the workspace `target/` folder, or `$CARGO_TARGET_DIR`
if you set it):

```sh
codesign --force --sign "$TETHER_DEV_SIGN_IDENTITY" --identifier ca.clym.termlyx path/to/target/debug/tether-app
```

- `ambiguous`: two certificates share the name; use the ID (step 4).
- Hangs: a "codesign wants to access key" dialog is waiting; do step 3, or
  `pkill -x codesign` and then step 3.
- Prompts on every rebuild although signing works: check `TeamIdentifier`
  above; a self-signed certificate shows `not set` and cannot help.

## Turn it off

`unset TETHER_DEV_SIGN_IDENTITY` (and remove the line from your shell
profile). Rebuilds go back to ad-hoc signing and prompting.
