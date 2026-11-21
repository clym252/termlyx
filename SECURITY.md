# Security

## Report a vulnerability

Use GitHub private vulnerability reporting:
<https://github.com/clym252/termlyx/security/advisories/new>

Do not open a public issue for a security problem, and do not post secrets,
vault files or setup codes anywhere. Include the version (Settings → About),
the platform and the steps to reproduce.

## Supported versions

Only the latest 0.x release gets fixes. Termlyx is in beta; update first.

## Design

How the vault and sync are protected:

- [docs/crypto.md](docs/crypto.md): the vault cryptography (format v1).
- [docs/protocol.md](docs/protocol.md): the sync protocol (v1).

The app has no telemetry. The sync server stores only ciphertext, wrapped keys
and routing metadata.

## Verify a download

Releases carry `SHA256SUMS`, build-provenance attestations and a signed server
image; see "Verify your download" in [docs/self-host.md](docs/self-host.md).
