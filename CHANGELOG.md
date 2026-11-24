# Changelog

Versions are plain `X.Y.Z`, the same on every platform. Termlyx is in beta
while the version starts with 0.

## 0.1.1 (2026-11-24)

Security fix for sync. Update every device that syncs.

- Sync: a record from the server is now stored only if it decrypts with your
  vault key. A server could otherwise swap one record's encrypted contents
  for another's, or for garbage. Rejected records are logged and skipped, and
  an unsynced local edit of the same record wins. Tombstones and replays of
  older versions are not checked yet; see
  [docs/crypto.md](docs/crypto.md#what-a-dishonest-server-can-do).
- Dependencies: cookie 0.18.3 (RUSTSEC-2026-0594).
- Build: Rust 1.100.0; the server image builds on `rust:1.100.0-trixie`.

## 0.1.0 (2026-11-23)

First public beta: SSH terminal workspace, dual-pane SFTP, port forwarding,
encrypted vault, optional self-hosted end-to-end-encrypted sync, signed Mac
app and unsigned Windows installer, in-app update check.

- macOS: universal DMG, Developer ID signed, notarized and stapled.
- Windows 10/11 x64: per-user installer, not code-signed yet.
- Sync server: Docker image `ghcr.io/clym252/termlyx-server` (signed with
  cosign) and static Linux binaries for x86_64 and aarch64.
- iOS and Android builds are not distributed yet.
