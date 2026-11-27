# Building

## Prerequisites

- Rust stable (pinned by `rust-toolchain.toml`; `rustup` installs it).
- Node 22 and pnpm.
- Docker, only for the SSH fixture and the server image.
- The [Tauri 2 system prerequisites](https://v2.tauri.app/start/prerequisites/)
  for your OS:
  - macOS: Xcode command line tools (Xcode 16+ for iOS).
  - Windows: Microsoft C++ Build Tools, WebView2, and NASM and CMake (the SSH
    crypto backend, aws-lc-rs, builds C code).
  - Linux: the WebKitGTK and build packages Tauri lists.

## Build and test

```sh
pnpm -C apps/tauri install

cargo test --workspace                 # fixture-backed tests skip without TETHER_SSH_FIXTURE=1
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check

pnpm -C apps/tauri exec tsc
pnpm -C apps/tauri vitest run          # frontend unit tests
pnpm -C apps/tauri playwright test     # end-to-end against the browser mock

pnpm -C apps/tauri tauri dev           # run the app
pnpm -C apps/tauri tauri build         # a release bundle for your OS
```

`./scripts/acceptance.sh [step ...]` runs the full gate (fixture, cargo,
throughput, vitest, playwright, server, sync, canary). It needs Docker.

## Local settings for the helper scripts

`scripts/devvm.sh`, `win.sh`, `android.sh`, `clean-cache.sh` and `ios.sh` read machine-specific
settings from `~/.config/termlyx/local.sh` (override the path with `TERMLYX_LOCAL_CONFIG`), a
shell file of `export` lines. Variables already set in the environment win. Names:
`TETHER_DEVVM_HOST` and `TETHER_WIN_HOST` (`user@host`), `TETHER_DEVVM_KEY` and `TETHER_WIN_KEY`
(ssh key paths), `CARGO_TARGET_DIR`, `APPLE_DEVELOPMENT_TEAM` (iOS Team ID).

## SSH test fixture

A disposable OpenSSH server in Docker, for integration tests only. It listens
on loopback (`127.0.0.1:2222`) and nowhere else.

```sh
./tests/fixtures/up.sh
TETHER_SSH_FIXTURE=1 cargo test --workspace
./tests/fixtures/down.sh
```

**Test-only credentials.** The login `tether` / `tether-test` and the key
`tests/fixtures/keys/id_ed25519_test` are public and committed on purpose.
They protect nothing. Never use them on a real host, and never expose the
fixture beyond loopback. Details: `tests/fixtures/README.md`.

## Sync server

```sh
export TERMLYX_DATA=$PWD/.tether-data TERMLYX_BIND=127.0.0.1:8443 \
  TERMLYX_NAMES=localhost TERMLYX_URL=https://localhost:8443
cargo run -p tether-server -- serve
```

To run one for real, see [docs/self-host.md](docs/self-host.md).

## iOS and Android

Short form; iOS notes are in [docs/ios-dev.md](docs/ios-dev.md).

- iOS (macOS only): `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`,
  then `pnpm -C apps/tauri tauri ios build --target aarch64-sim --debug`.
- Android: install the Android SDK and NDK, add the Rust Android targets, then
  `pnpm -C apps/tauri tauri android build`.

The desktop updater is desktop-only; mobile builds come from the stores.

## Dev signing on macOS (optional)

Keychain asks again after every dev rebuild; signing dev builds with your own
Apple Development certificate stops that. See
[docs/dev-signing.md](docs/dev-signing.md).

## Pre-push scan hook (maintainers only)

Contributors do not need this: you do not have the private pattern file, and
CI runs gitleaks for everyone and skips the personal scan only for fork PRs
and Dependabot runs.

Runs gitleaks, then the personal-info scan, before every push.

```sh
git config core.hooksPath scripts/hooks
export TERMLYX_PERSONAL_PATTERNS=/path/to/private/patterns   # one regex per line
```

Needs `gitleaks` on `PATH`. If the pattern file is missing the hook fails
(exit 2) instead of passing silently. CI runs the personal scan only on pushes
to `main` (the patterns are a `personal-scan` environment secret that pull
requests cannot reach), so the hook is what checks a branch before it is
pushed; on `main` an unset secret fails the run.

The real pattern file must not contain the synthetic word used in
`scripts/scan-personal.test.sh`, or that file matches itself.
