# Termlyx

Termlyx is a clean-room, self-hosted SSH client: a Rust core with a Tauri 2 +
React 19 + xterm.js app for Mac and Windows, with iOS and Android builds.

> **Beta: expect bugs; back up your vault.**

## Features

- Terminal workspace with tabs, host groups and snippets.
- Dual-pane SFTP file manager.
- Local, remote and dynamic port forwarding.
- Encrypted vault for hosts, keys and passwords; known-host verification;
  quick unlock through the system keychain.
- Optional end-to-end-encrypted sync between your own devices, through a
  server you run yourself (one binary or one Docker image).
- In-app update check that asks before installing anything.

## Download

Get the latest build from the
[releases page](https://github.com/clym252/termlyx/releases/latest).

- [Install on macOS](docs/install-macos.md): universal, signed and notarized.
- [Install on Windows](docs/install-windows.md): the beta installer is not
  code-signed, so SmartScreen asks for one extra click.
- Verify a download: see "Verify your download" in [docs/self-host.md](docs/self-host.md).

## Self-host the sync server

Sync needs a Termlyx server you run yourself (Docker required). One command:

```sh
# Linux or macOS
curl -fsSL https://github.com/clym252/termlyx/releases/latest/download/install.sh | sh
```

```powershell
# Windows (Docker Desktop)
irm https://github.com/clym252/termlyx/releases/latest/download/install.ps1 | iex
```

It prints a setup code and the server's address. Then in the app:
Settings → Sync → Connect to server. Manual install, add-device, backup, VPN
and troubleshooting are in [docs/self-host.md](docs/self-host.md).

## Privacy

No telemetry. Your vault is encrypted on your device; the sync server stores
only ciphertext. How: [docs/crypto.md](docs/crypto.md) and
[docs/protocol.md](docs/protocol.md).

## Build from source

See [BUILDING.md](BUILDING.md). Contributions are welcome:
[CONTRIBUTING.md](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md).
Maintainers: [RELEASING.md](RELEASING.md). Changes: [CHANGELOG.md](CHANGELOG.md).

## Security

Report vulnerabilities privately: see [SECURITY.md](SECURITY.md).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

Tether was the working name; internal identifiers keep it.
