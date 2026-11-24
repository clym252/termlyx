> **Beta: expect bugs; back up your vault.**

**Which file do I need?**

| You want | Download | Guide |
| --- | --- | --- |
| The Mac app | `Termlyx_<version>_universal.dmg` | [Install on macOS](https://github.com/clym252/termlyx/blob/main/docs/install-macos.md) |
| The Windows app | `Termlyx_<version>_x64-setup.exe` | [Install on Windows](https://github.com/clym252/termlyx/blob/main/docs/install-windows.md) |
| The sync server | nothing: run the one-line install command | [Self-host the server](https://github.com/clym252/termlyx/blob/main/docs/self-host.md) |

The other files are used by the in-app updater (`latest.json`, `.tar.gz`,
`.sig`), the install scripts (`install.sh`, `install.ps1`, `compose.yml`),
Linux installs without Docker (`termlyx-server-*`, `termlyx-server.service`)
and verification (`SHA256SUMS`, the SBOM).

**Verify your download:** every file is listed in `SHA256SUMS` and has a
build-provenance attestation (`gh attestation verify <file> --repo clym252/termlyx`).
Do this before you open the Windows installer.

**macOS:** the app is signed with a Developer ID and notarized by Apple.

**Windows:** the installer is not code-signed yet, so SmartScreen may warn
"Windows protected your PC". SmartScreen shows the same warning for a
tampered copy, so verify the file first (above). Then click **More info**,
then **Run anyway**.
