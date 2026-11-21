# Test fixtures

## SSH server (`docker-compose.yml`)

A disposable OpenSSH server used by the integration tests.

| Item     | Value                                              |
|----------|----------------------------------------------------|
| Address  | `127.0.0.1:2222`                                   |
| User     | `tether`                                           |
| Password | `tether-test`                                      |
| Key      | `keys/id_ed25519_test` (authorized via `.pub`)     |

```sh
eval "$(scripts/devvm.sh up)"   # tunnel to devvm + DOCKER_HOST (see below)
./tests/fixtures/up.sh     # start (idempotent) and wait until ready
./tests/fixtures/down.sh   # stop and remove
docker compose -f tests/fixtures/docker-compose.yml restart sshd   # used by reconnect tests
```

Integration tests only run when `TETHER_SSH_FIXTURE=1` is set. They need
`DOCKER_HOST` from the `eval` above in the same shell (they run `docker exec`).

## Docker on devvm (`scripts/devvm.sh`)

Preferred: run the tests on devvm itself, so the Mac neither builds nor runs
them. devvm has its own checkout (`~/src/termlyx`), cargo cache and Docker:

```sh
scripts/devvm.sh sync                        # committed HEAD -> devvm (commit first)
scripts/devvm.sh run fixture cargo server sync   # acceptance.sh there, TETHER_DOCKER=local
```

Output streams to the terminal and is saved to
`$CARGO_TARGET_DIR/devvm-logs/acceptance.log`. Ctrl-C ends the Mac side; the
remote run stops at its next write (`ssh "$TETHER_DEVVM_HOST" pkill -f
acceptance.sh` if not). devvm setup (2026-10-28): Ubuntu 26.04, Docker +
Compose, `apt-get install pkg-config libssl-dev libdbus-1-dev
libwebkit2gtk-4.1-dev libxdo-dev libayatana-appindicator3-dev librsvg2-dev
file curl`, rustup (minimal). Node + pnpm are in
`/usr/local/node/bin`; `vitest`/`playwright`/`canary` there are not set up.

Otherwise, the Mac can drive devvm's containers through a tunnel:

The fixture and the acceptance sync server (`tether-accept`) run in Docker on
the Linux build host, not Docker Desktop on the Mac (its VM
held 8 of the Mac's 16 GB). The tests are unchanged: one background SSH
connection from the Mac forwards

| Mac                                | devvm                    |
|------------------------------------|--------------------------|
| `localhost:2222`                   | `127.0.0.1:2222` (sshd)  |
| `localhost:18443`                  | `127.0.0.1:18443` (`acceptance.sh server`) |
| `~/.cache/tether-devvm/docker.sock` | `/var/run/docker.sock`  |

and `DOCKER_HOST=unix://~/.cache/tether-devvm/docker.sock` makes the Mac's
`docker` CLI (compose, `exec`, `cp`, image builds; the build context is sent
over the socket) drive devvm's daemon.

```sh
eval "$(scripts/devvm.sh up)"   # open (no-op if open) and export DOCKER_HOST
scripts/devvm.sh status         # exit 0 if open
scripts/devvm.sh down           # close; containers on devvm keep running
```

- `scripts/acceptance.sh` does this itself for the steps that need Docker
  and closes the tunnel on exit if it opened it. `scripts/win.sh run fixture`
  likewise.
- Fallback: `TETHER_DOCKER=local` makes both scripts use the Mac's own Docker
  (start Docker Desktop first; `scripts/devvm.sh down` if the tunnel holds
  2222/18443). By hand: just don't run the `eval` (or `unset DOCKER_HOST`).
- Env: `TETHER_DEVVM_HOST` (required, `user@host`), `TETHER_DEVVM_KEY`
  (default `~/.ssh/id_ed25519`). The host needs Docker with
  Compose v2 and the user in the `docker` group; nothing else is installed.
- No bind mounts: the remote daemon cannot see this checkout, so the
  Dockerfile copies the public key and `sshd-init/` into the image.
- The forwarded Docker socket is root on devvm for any process of this Mac
  user (socket 0600 in a 0700 directory). Both published ports stay on
  devvm's loopback.
- Images build on devvm, so they are linux/amd64 (the Mac is arm64); the
  tests only talk to them over the network.
- `tests/fixtures/up.sh` fails with "no Docker daemon answers" when
  `DOCKER_HOST` is unset and Docker Desktop is not running.
- "could not open the devvm tunnel": devvm down, or 2222/18443 already bound
  on the Mac (the Mac's own fixture, or a tunnel from another user).

### tmux and environment variables

- The sshd image is built from `Dockerfile`: the pinned linuxserver image
  plus `tmux` and `fish` (`apk add`; fish is a login shell with non-POSIX
  quoting, which the startup-command tests run through), tagged `tether-fixture-sshd:...-tmux`. `up.sh`
  runs `docker compose up -d --build`, so the first start builds it. After
  editing the Dockerfile, run `down.sh` then `up.sh`. Remove the image with
  `docker rmi tether-fixture-sshd:10.3_p1-r1-ls239-tmux`.
- The image also holds `keys/id_ed25519_test.pub` (at `/keys`, for
  `PUBLIC_KEY_FILE`) and `sshd-init/` (at `/custom-cont-init.d`); the private
  key is kept out of the build context (`.dockerignore`).
- `sshd-init/50-tether-forwarding.sh` also appends `AcceptEnv LANG LC_*`
  (the stock config has none), so a client's per-channel `env` request
  (`LC_TETHER_TAB`) reaches the shell. The login shell of `tether` is bash.

### Port forwarding

- sshd allows TCP forwarding with `AllowTcpForwarding yes` and
  `GatewayPorts clientspecified`. The image writes its own `sshd_config` at
  startup, so `sshd-init/50-tether-forwarding.sh` is copied to
  `/custom-cont-init.d`. The image runs it after its own init and before
  sshd starts. The script is idempotent.
- `web` is `traefik/whoami` (pinned). It runs on the `internal` network, so
  only sshd can reach it, as `web:80`. It answers HTTP with its `Hostname:`
  and the request.
- `up.sh` also checks that forwarding is on and that `web:80` answers from
  inside `tether-sshd` (it uses `curl` in the container).
- Remote-forward tests connect from inside the container with OpenBSD `nc`
  (`docker exec tether-sshd nc -N ...`). `-N` half-closes after stdin ends.

Quick check (git does not preserve the 0600 mode; `up.sh` also does this):

```sh
chmod 600 tests/fixtures/keys/id_ed25519_test
ssh -p 2222 -i tests/fixtures/keys/id_ed25519_test \
  -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=no \
  tether@127.0.0.1 echo ok
```

## WARNING: test-only credentials

`keys/id_ed25519_test` is a **private key committed to the repository on
purpose**. It, and the `tether` / `tether-test` password, exist only for this
throwaway test container (on devvm's loopback, or the Mac's). They are public. **Never use them, or any key
derived from them, for a real host.** Never authorize this key anywhere
outside the fixture.
