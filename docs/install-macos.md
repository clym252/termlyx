# Install Termlyx on macOS

The Mac app is a universal build (Apple silicon and Intel). It is signed with a
Developer ID and notarized by Apple, so it opens without a Gatekeeper warning.

## Install

1. Open the [latest release](https://github.com/clym252/termlyx/releases/latest)
   and download `Termlyx_<version>_universal.dmg`.
2. Optional: check it against `SHA256SUMS` (see "Verify your download" in
   [self-host.md](self-host.md)).
3. Open the DMG and drag **Termlyx** onto **Applications**.
4. Open Termlyx from Applications. On first launch macOS says it was
   downloaded from the internet; click **Open**.
5. Create your master password. It encrypts your vault and cannot be
   recovered; keep the recovery key somewhere safe.

Termlyx is a beta: expect bugs and back up your vault.

## Update

Settings → About → **Check for updates**. Termlyx also checks on its own
unless you turn that off in the same place. It never installs without your
click, and it asks first if sessions are open. Or download the new DMG and
replace the app.

## Uninstall and your data

Drag Termlyx from Applications to the Trash. Your vault stays until you
delete it:

- `~/Library/Application Support/ca.clym.termlyx/` (the vault, `vault.sqlite`)
- `~/Library/WebKit/ca.clym.termlyx/` (device settings)
- Keychain items Termlyx created (quick unlock), in Keychain Access

Delete the first two folders to remove all local data. Synced data stays on
your own server until you remove it there.
