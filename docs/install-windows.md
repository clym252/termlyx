# Install Termlyx on Windows

Windows 10 or 11, 64-bit. The installer installs for your user only; no
administrator rights are needed.

## Install

1. Open the [latest release](https://github.com/clym252/termlyx/releases/latest)
   and download `Termlyx_<version>_x64-setup.exe`. If Edge says the file
   **isn't commonly downloaded**, click **…** next to it, then **Keep**, then
   **Show more** → **Keep anyway**.
2. Optional: check it against `SHA256SUMS` (see "Verify your download" in
   [self-host.md](self-host.md)).
3. Run it. The beta installer is not code-signed (a signing certificate costs
   money and the project has none yet), so SmartScreen may warn **Windows
   protected your PC**. Click **More info**, then **Run anyway**.
4. Termlyx installs to `%LOCALAPPDATA%\Termlyx` and starts.
5. Create your master password. It encrypts your vault and cannot be
   recovered; keep the recovery key somewhere safe.

The download is still verifiable: compare its SHA-256 with `SHA256SUMS` or run
`gh attestation verify` as described in [self-host.md](self-host.md).

Termlyx is a beta: expect bugs and back up your vault.

## Update

Settings → About → **Check for updates**. Termlyx also checks on its own
unless you turn that off in the same place. It never installs without your
click, and it asks first if sessions are open. The update uses the same
per-user installer.

## Uninstall and your data

Settings → Apps → Installed apps → Termlyx → Uninstall. Your vault stays until
you delete it:

- `%APPDATA%\ca.clym.termlyx\` (the vault, `vault.sqlite`)
- `%LOCALAPPDATA%\ca.clym.termlyx\` (device settings)
- Windows Credential Manager entries Termlyx created (quick unlock)

Delete the two folders to remove all local data. Synced data stays on your own
server until you remove it there.
