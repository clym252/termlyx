# Contributing

Termlyx is a clean-room project. Do not copy code, assets or text from any proprietary SSH client.

By contributing you agree your work is licensed under Apache-2.0 (see
[LICENSE](LICENSE)). Please follow the [Code of Conduct](CODE_OF_CONDUCT.md).
For a security problem, see [SECURITY.md](SECURITY.md) instead of opening an
issue.

## Pull request flow

1. Open an issue first for anything larger than a small fix.
2. Fork, branch from `main`, keep the change focused.
3. Add or update tests for what you change; run them (below).
4. Sign off every commit (DCO): `git commit -s`. This adds a
   `Signed-off-by:` line certifying you wrote the change or may submit it
   under the project license (<https://developercertificate.org/>).
5. Open the PR against `main`.

Never put secrets, keys, passwords or personal data in code, tests, logs or
screenshots.

## Running the tests

You need no private infrastructure. Everything runs on your machine; setup is
in [BUILDING.md](BUILDING.md).

```sh
cargo test --workspace                 # fixture-backed tests skip without TETHER_SSH_FIXTURE=1
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
pnpm -C apps/tauri exec tsc
pnpm -C apps/tauri vitest run          # frontend unit tests
pnpm -C apps/tauri playwright test     # end-to-end against the browser mock
```

For the SSH integration tests, start the local fixture (needs Docker):

```sh
./tests/fixtures/up.sh
TETHER_SSH_FIXTURE=1 cargo test --workspace
./tests/fixtures/down.sh
```

The `tether-*` crate names and `TETHER_*` variables are the project's former
working name and stay as they are.
