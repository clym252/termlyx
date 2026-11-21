# Termlyx

Self-hosted SSH client: Rust core, Tauri 2 + React 19 + xterm.js app, and a
self-hosted end-to-end encrypted sync server. `README.md` covers features and
downloads; `BUILDING.md` covers setup.

Tether was the working name; internal identifiers (crate names, `TETHER_*`
env vars, keychain services, wire and crypto strings) keep it on purpose.
Do not rename them: several are part of the frozen vault format.

## Hard rules

- Clean room: never copy code, assets or text from any proprietary SSH client.
- No secrets (passwords, keys, tokens, vault keys) in logs, `Debug` output,
  DTOs or React state.
- Never push, tag or publish a release without the maintainer's explicit
  word, each time.
- Never use bare `git stash` / `git stash pop` (the stack is shared by all
  worktrees). Use a WIP commit, or `git stash push -u -m <tag>` and apply by SHA.
- Never change keychain or OS security settings for the user. Give them the
  exact commands to run themselves.
- `docs/crypto.md` (format v1) and `docs/protocol.md` (sync v1) are frozen:
  changes are additive only.

## Build and test

```sh
cargo test --workspace                 # Rust; fixture-backed tests skip without TETHER_SSH_FIXTURE=1
tests/fixtures/up.sh                   # Docker OpenSSH fixture on 127.0.0.1:2222
pnpm -C apps/tauri install
pnpm -C apps/tauri vitest run          # frontend unit tests
pnpm -C apps/tauri playwright test     # e2e against the browser mock
pnpm -C apps/tauri tauri dev           # run the app
./scripts/acceptance.sh [step ...]     # full gate: fixture cargo throughput vitest playwright server sync canary
scripts/scan-personal.sh               # personal-string scan (maintainers; needs the private pattern file)
```

## Map

- `crates/` core-model (records, HLC) · core-crypto (vault crypto) · core-store
  (SQLite of sealed envelopes) · core-proto (sync wire types) · core-sync (sync
  client) · core-ssh (russh transport, SFTP, forwarding) · core-api (sessions,
  vault services)
- `apps/tauri/` `src/` React frontend (+ `api/mock.ts` browser mock);
  `src-tauri/` Tauri commands; `src-tauri/gen/apple` committed iOS project
- `server/` termlyx-server: sync server, one binary + SQLite, Docker
- `deploy/` compose file and installers for self-hosting
- `tests/fixtures/` Docker OpenSSH fixture
- `scripts/` acceptance gate, release tooling (`scripts/release/`), dev-VM helpers
- `vendor/russh` patched russh
- `docs/` protocol.md (sync v1) · crypto.md (frozen format v1) · self-host.md
  · install guides · dev-signing.md · ios-dev.md · store-release.md
