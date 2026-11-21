# Vendored russh 0.64.1

Source: the crates.io release `russh` 0.64.1, copied unchanged from the cargo
registry (`~/.cargo/registry/src/*/russh-0.64.1`; `.cargo-ok` dropped).

- crates.io checksum: `ba61e87b9ec9a39a59a6bbed4c0b8ff7fb07073405b24a767a1f2c8560ac8f58`
- upstream commit (`.cargo_vcs_info.json`): `e5d80a07dc554480b6ea3b2e14adfc190407b627`
  (warp-tech/russh `main` HEAD as of 2026-10-19)
- License: Apache-2.0. The crate package ships no license file, so
  `LICENSE.txt` is copied from the upstream repository root at that commit.

Wired in with `[patch.crates-io] russh = { path = "vendor/russh" }` in the
workspace `Cargo.toml`, and listed under `workspace.exclude` so its tests,
benches and examples never build as part of the workspace. Sibling crates
(`russh-cryptovec`, `russh-util`) are unchanged and still come from crates.io.

## Tether patch: opt-in per-channel window control

The full diff against the pristine copy is in `tether-window.patch` (about
160 added lines in `src/`, about 60 of them a unit test, plus one integration test
file), apart from the modification notices below.

### Why

Stock russh 0.64.1 tops up a channel's receive window when data **arrives**,
not when the application **consumes** it. Two places do this:

- `src/client/encrypted.rs` (`CHANNEL_DATA` at about line 577,
  `CHANNEL_EXTENDED_DATA` at about line 604) calls `adjust_window_size` with
  `config.window_size` for every packet.
- `src/session.rs` `adjust_window_size` sends `CHANNEL_WINDOW_ADJUST` back up
  to `window_size` once the window is below half.

The session loop then hands the data to the channel with an awaited, bounded
`chan.send(..)` (`client/encrypted.rs`, about line 589). As a result:

- If one channel's reader stops, the loop blocks and every channel on the
  connection freezes.
- The only alternative is to buffer the data without any bound.

Tether puts shells, SFTP and port forwards on one connection, so it needs
per-channel flow control. The public API had no way to hold the window back.
`Handler::adjust_window`'s return value is stored but never read on the client
side.

### What

- `client::Config::manual_window_adjust: bool`, default `false`.
  - When `true`, incoming `CHANNEL_DATA` and `CHANNEL_EXTENDED_DATA` only
    decrement the channel's window (`Encrypted::consume_window`), and nothing
    refills it.
  - When `false`, the code path is exactly stock: the same
    `adjust_window_size` call and the same `Handler::adjust_window` hook.
- `client::Handle::window_granter()` returns a `client::WindowGranter`. It
  is cheap to clone and does not keep the session alive.
  `WindowGranter::adjust_window(channel, bytes)` never waits: it puts the new
  `ChannelMsg::AdjustWindow { bytes }` on the session's **priority** queue
  (the enum is already `#[non_exhaustive]`).
  - `Handle` keeps a clone of the session's existing `priority_sender`.
  - The client session loop handles the message in
    `client::Session::adjust_window`, which calls `Encrypted::grant_window`.
  - Why the priority queue: the loop takes from the normal outbound queue
    only while `can_receive_outbound` holds (`!kex.active() &&
    !has_any_pending_data()`, `src/client/mod.rs`, around line 1376). While
    any channel's outbound data waits on the server's window, grants sent
    there would wait too, and every other channel would stop after one
    window. The priority queue is served whenever no key exchange is
    running.
  - The first version of this patch put `adjust_window` on `Channel` and
    `ChannelWriteHalf` (normal queue) and had exactly that bug; review found
    it.
  - `grant_window` sends `CHANNEL_WINDOW_ADJUST(bytes)`, capped so the window
    stays at or below 2^32-1, and ignores unconfirmed or unknown channels.
- `server/session.rs` ignores `AdjustWindow`, so the public API cannot reach
  the server's `unimplemented!` arm.
- New test: `tests/test_manual_window.rs` (registered in `Cargo.toml`). Two
  channels each get exactly one window. Granting window on one channel moves
  only that channel, and the other resumes once it is granted window. The test
  fails with the flag off.
- New unit test: `client::tests::window_grants_bypass_a_write_blocked_channel`.
  With one channel holding pending outbound data (normal queue closed), a
  grant for another channel is taken from the priority queue and queues its
  `CHANNEL_WINDOW_ADJUST`. A russh server always refills its windows, so it
  cannot model a peer that stops granting; Tether's
  `write_blocked_shell_does_not_stall_other_channels` fixture test covers the
  end-to-end case against OpenSSH.

### Known limitation (stock, unchanged)

While any channel's outbound data waits on the server's window, the client
session loop does not take messages from the normal outbound queue. So every
channel's writes, channel opens, requests and closes wait until the server
grants that channel more window. This is stock russh behaviour and the patch
leaves it alone; only window grants bypass it.

Files touched: `Cargo.toml` (test target), `src/channels/mod.rs`,
`src/client/encrypted.rs`, `src/client/mod.rs`, `src/client/session.rs`,
`src/server/session.rs`, `src/session.rs`.

### Modification notices (Apache-2.0 section 4(b))

Each modified file starts with a one-line notice, `// Modified by Tether
(see TETHER-PATCH.md).` (`#` in `Cargo.toml`). The notices are comments only
and are not part of `tether-window.patch`; re-add them after re-applying the
patch. Files carrying the notice:

- `Cargo.toml`
- `src/channels/mod.rs`
- `src/client/encrypted.rs`
- `src/client/mod.rs`
- `src/client/session.rs`
- `src/server/session.rs`
- `src/session.rs`

`tests/test_manual_window.rs` is new (Tether's own file), not a modified one.

Verified on 2026-10-19. The crate's full suite passes with the patch: lib
tests (206) and every integration test, including the new ones. It was run as
`cargo test --locked --lib --tests` from a copy outside this repository,
because the worktree sits inside another cargo workspace.

### Dropping the patch

Upstream main (`e5d80a07`, the 0.64.1 release commit) has no equivalent API.
The API shape is meant to be upstreamable. When upstream gains
consumer-driven window control:

1. Point `core-ssh` at the release and delete `[patch.crates-io]`,
   `workspace.exclude` and `vendor/russh`.
2. Adapt `crates/core-ssh/src/connection.rs`. It uses only the
   `manual_window_adjust` config flag and
   `Handle::window_granter`/`WindowGranter::adjust_window`.

When bumping russh without such an API, re-vendor the new release and
re-apply `tether-window.patch`.
