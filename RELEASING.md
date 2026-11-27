# Releasing

For maintainers. Releases are built by `.github/workflows/release.yml` from a
`vX.Y.Z` tag. Versions are plain `X.Y.Z`, the same on every platform; the
first real one is `0.1.0`. Releases are not marked Pre-release (the updater
and install scripts use `releases/latest`, which skips pre-releases); the 0.x
version and the notes banner say "beta".

## Release flow

1. `scripts/bump-version.sh X.Y.Z`, update `CHANGELOG.md`, commit.
2. Tag `vX.Y.Z` on that commit. A tag that does not match every version in the
   files fails the `check` job before any build.
3. Push the tag (the maintainer only; the tag ruleset allows nobody else).
4. The workflow checks, then builds the Mac app (universal, signed,
   notarized, stapled), the Windows installer (unsigned NSIS), the server
   image (`ghcr.io/clym252/termlyx-server:vX.Y.Z`, cosign keyless, SBOM,
   provenance) and Linux server binaries. The Mac and Windows jobs wait for
   approval of the `release` environment: review the run, then approve.
5. A **draft** release appears with the assets below. Check it:
   - assets: `Termlyx_X.Y.Z_universal.dmg` (notarized and stapled), `Termlyx.app.tar.gz` and `.sig`,
     `Termlyx_X.Y.Z_x64-setup.exe` and `.sig`, the versionless copies
     `Termlyx_universal.dmg` and `Termlyx_x64-setup.exe` (the website links to
     them via `releases/latest/download/`), `termlyx-server-<target>`,
     `latest.json`, `SHA256SUMS`, `termlyx-X.Y.Z-sbom.cdx.json`, and the
     server deploy files;
   - `latest.json` lists both Mac and Windows with signatures;
   - the beta banner heads the notes.
6. Publish the draft. Publishing runs `publish-latest.yml`, which moves the
   server image's `:latest` tag. The updater endpoint
   (`releases/latest/download/latest.json`) goes live at this point.
   If no `publish latest image` run starts within a few minutes, run it by
   hand: `gh workflow run publish-latest.yml -f tag=vX.Y.Z`.

### Post-release smoke test

On a clean Mac and a clean Windows machine, on the previous version if there
is one:

1. Install from the release page (Windows: SmartScreen **More info**, **Run
   anyway**). The Mac app opens with no Gatekeeper warning.
2. `shasum -a 256` (or `Get-FileHash`) matches `SHA256SUMS`;
   `gh attestation verify <file> --repo clym252/termlyx` passes.
3. Settings → About shows the new version.
4. From the previous version: Settings → About → Check for updates offers the
   new one; install it; the app restarts on the new version with the vault
   intact.
5. `cosign verify` and `docker pull` of the server image work (see
   [docs/self-host.md](docs/self-host.md)); `docker run` starts it.

## Credentials

Every signing secret lives in the GitHub environment `release` as an
**environment secret**, not a repo secret. The environment requires the
maintainer's approval for each run and is limited to `v*` tags. The desktop
jobs compile first with no secrets in the environment (`tauri build
--no-bundle`, which runs the frontend build and every `build.rs`); only the
`tauri bundle` step that signs, notarizes and writes the updater signature
gets them. The exception
is `PERSONAL_SCAN_PATTERNS`, a secret of the `personal-scan` environment
(deployable from `main` only, no reviewer), used by CI's personal scan on
pushes to `main`. Pull requests never see it; the pre-push hook covers them.

| Credential | Secret name(s) | Lives in | Used by | Backup |
|---|---|---|---|---|
| Developer ID Application certificate (p12) and its password | `APPLE_CERTIFICATE` (base64 p12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` (the certificate's common name) | maintainer's login keychain; exported p12 in env `release` | Mac release job | password manager |
| App Store Connect API key (.p8), key id, issuer id | `APPLE_API_KEY_P8` (the .p8 text), `APPLE_API_KEY` (key id), `APPLE_API_ISSUER` | env `release` | Mac notarization | password manager (Apple shows the .p8 once) |
| Updater signing key (minisign) and password | `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; public key in `apps/tauri/src-tauri/tauri.conf.json` | env `release` | Mac and Windows release jobs | password manager **and** an offline copy. Losing it strands installed apps |
| Personal-info scan patterns | `PERSONAL_SCAN_PATTERNS` | env `personal-scan` (not a repo secret); the file itself stays private | CI `personal scan` job (pushes to `main`) | private copy |
| `GITHUB_TOKEN` | automatic | each job | image push, release creation | none |
| cosign | none: keyless (OIDC) | none | image signing | none |

### Developer ID certificate

- **Create:** in Apple Developer, Certificates, create a "Developer ID
  Application" certificate and install it in the login keychain. In Keychain
  Access export it with its private key as a .p12 with a password; store
  both in the password manager. `base64 -i cert.p12 | pbcopy` gives the
  `APPLE_CERTIFICATE` value.
- **Rotate:** create a new certificate, export, update the three secrets, and
  cut a release. Old releases stay valid; the old certificate can expire.
- **Leak or loss:** revoke the certificate in Apple Developer (apps signed
  with it stop passing Gatekeeper on new installs after revocation), create a
  new one, update the secrets, and release again. Losing only the p12 copy
  while the keychain has it is recoverable by re-exporting.

### App Store Connect API key

- **Create:** App Store Connect, Users and Access, Integrations, Team Keys.
  Download the .p8 immediately; note the key id and issuer id.
- **Rotate:** create a new key, update `APPLE_API_KEY_P8`, `APPLE_API_KEY` and
  `APPLE_API_ISSUER`, then revoke the old one.
- **Leak or loss:** revoke it in App Store Connect. Notarization fails until a
  new key is in the environment; nothing already shipped is affected.

### Updater signing key

Generate with `pnpm -C apps/tauri tauri signer generate -w <file>`. The public
key goes into `tauri.conf.json`; the private key and password go into the
environment and into the password manager, plus an offline copy.

An installed app trusts exactly one updater key (the single `pubkey` in
`tauri.conf.json`); there is no multi-key support.

- **Planned rotation (bridge release):**
  1. Generate the new key.
  2. Ship one release signed with the **old** key whose `tauri.conf.json`
     carries the **new** public key.
  3. Every release after it is signed with the new key.
  4. Keep the old key until the bridge release is out and adopted (installed
     apps only follow later updates once they have taken it).
  Signing the first new-key release before the bridge is adopted strands the
  apps that have not taken it.
- **Lost or leaked old key:** installed apps cannot be moved safely. Users must
  download and reinstall from the release page. Tell them in the release
  notes and the README (a banner at the top of both). A leaked key also lets
  anyone who can publish at the update endpoint sign an update, so lock the
  repo first. This is why backing up the key (password manager plus an offline
  copy) is a release blocker.

### GITHUB_TOKEN and cosign

`GITHUB_TOKEN` is created per job with the permissions each job declares; there
is nothing to rotate. cosign signing is keyless: the signature is tied to this
repository's workflow identity through GitHub's OIDC token, and nothing is
stored. If the repository is renamed or moved, update the identity in the
`cosign verify` command in `docs/self-host.md`.

### PERSONAL_SCAN_PATTERNS

Create: put one regex per line in the private file, then
`gh secret set PERSONAL_SCAN_PATTERNS --env personal-scan < path/to/patterns`.
Rotate by setting it again. Never commit the file or paste its lines into
issues or logs. It must not also exist as a repo secret: a same-repo pull
request runs its own workflow file and could print a repo secret.

## Repo settings

Set once, check after any ownership change:

- secret scanning with push protection, and Dependabot alerts;
- private vulnerability reporting (SECURITY.md points to it);
- a ruleset on `main`: no force push, no deletion;
- a tag ruleset: only the maintainer creates `v*` tags;
- default workflow permissions read-only;
- approval required for workflows from fork PRs;
- the `release` environment: required reviewer, `v*` tags only;
- the `personal-scan` environment: deployment branch `main` only;
- the GHCR package `termlyx-server` set to Public (new packages start private,
  and `docker pull` fails for strangers until then).

The owner name `clym252` is written in `deploy/compose.yml`,
`deploy/install.sh`, `deploy/install.ps1`, `.github/workflows/release.yml`,
`docs/`, `README.md`, `server/README.md` and the app's `SELF_HOST_DOCS_URL`
(`apps/tauri/src/app/brand.ts`). Change them together if the owner changes;
GHCR names must be lowercase.

## Vendored russh: check upstream on each release

`vendor/russh` is russh with a small patch (`vendor/russh/TETHER-PATCH.md`,
diff in `vendor/russh/tether-window.patch`). Before each release:

1. Look at upstream russh for a new release or security advisory
   (<https://github.com/warp-tech/russh/releases>).
2. Diff the vendored copy against the pristine crates.io release it was taken
   from: the patch file is the expected difference; anything else is a defect.
3. If upstream moved, rebase the patch onto the new release, update
   `TETHER-PATCH.md` (version, checksum, commit), and run the cargo and
   fixture tests.
4. Run `cargo deny check` and the npm license check; `scripts/notices.sh`
   regenerates the third-party notices.
