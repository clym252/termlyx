# Changelog

Versions are plain `X.Y.Z`, the same on every platform. Termlyx is in beta
while the version starts with 0.

## 0.1.0 (2026-11-23)

First public beta: SSH terminal workspace, dual-pane SFTP, port forwarding,
encrypted vault, optional self-hosted end-to-end-encrypted sync, signed Mac
app and unsigned Windows installer, in-app update check.

- macOS: universal DMG, Developer ID signed, notarized and stapled.
- Windows 10/11 x64: per-user installer, not code-signed yet.
- Sync server: Docker image `ghcr.io/clym252/termlyx-server` (signed with
  cosign) and static Linux binaries for x86_64 and aarch64.
- iOS and Android builds are not distributed yet.
