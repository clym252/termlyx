# termlyx-server

The self-hosted Termlyx sync server. It stores ciphertext, wrapped keys and sync
metadata only: never a host name, password, key or label in the clear. One
binary, one SQLite database, packaged as a Docker image.

**Installing it? Read [`docs/self-host.md`](../docs/self-host.md).** This file is
the developer and advanced reference.

Settings are `TERMLYX_*` environment variables (or `/data/server.toml`).
Compatibility: the old `TETHER_*` names still work when the new one is unset,
and the developer deploy script and `server/compose.yml` below still take them.

## Build from source and deploy (developers)

This section is for working on the server. The release install in
`docs/self-host.md` uses port 8443 and `TERMLYX_*` settings; this developer
script keeps the old `TETHER_*` names and maps host port 443 by default.

The image is built on the target host from a trimmed copy of the source. From a
checkout of this repository (needs `ssh`, `tar` and `cargo` locally):

    scripts/deploy-server.sh user@host

The host is required; there is no default. `--dry-run` lists exactly what would
be sent and the commands that would run, without touching the host. The script
checks ssh and Docker Compose, sends the sources into `~/tether/src`, runs
`docker compose up -d --build`, waits up to two minutes for health, and prints
the certificate fingerprint and how to make the first invite. The build needs
about 4 GB of RAM (it is killed below roughly 3 GB of RAM plus swap; the script
warns but never changes your host) and about 5 GB of disk. The dev compose
service is called `tether`, so admin commands there are
`docker compose -f ~/tether/src/server/compose.yml exec tether termlyx-server ...`.

Settings are environment variables on the deploy command; the script saves the
ones you set in `~/tether/deploy.env` on the host, so later upgrades keep them
(edit that file to change or remove one):

| Variable | Meaning | Default |
| --- | --- | --- |
| `TETHER_NAMES` | DNS names in the certificate (comma list) | none (`termlyx`, `localhost`) |
| `TETHER_IPS` | IP addresses in the certificate (comma list) | none |
| `TETHER_URL` | Public URL put into full `tether1-` codes | unset |
| `TETHER_PORT` | Host port mapped to the container's 8443 | `443` |
| `TETHER_TLS`, `TETHER_TLS_CERT`, `TETHER_TLS_KEY`, `TETHER_ALLOW_PLAIN`, `TETHER_BIND` | See [TLS](#tls) | self-signed |

    TETHER_NAMES=termlyx.lan TETHER_IPS=192.168.1.20 scripts/deploy-server.sh me@192.168.1.20

`TETHER_URL` must carry the port when it is not 443, for example
`TETHER_PORT=8443 TETHER_URL=https://termlyx.lan:8443`. The server refuses to
put a URL into a code that clients would reject (plain `http://`).

## TLS

`TERMLYX_TLS` (environment or `/data/server.toml`) selects the mode:

- **`self-signed`** (default). On first start the server creates a P-256 key and
  a 20 year certificate for `TERMLYX_NAMES` / `TERMLYX_IPS` (or `termlyx` and
  `localhost`) under `/data/tls`. Clients pin the SHA-256 of the certificate's
  public key (shown by `termlyx-server fingerprint`), not the certificate, so
  renewing on the same key keeps the pin. Losing `/data/tls` changes the pin and
  clients refuse to connect ("certificate changed") until re-paired; back it up
  with `--with-tls`.
- **`files`**: `TERMLYX_TLS=files TERMLYX_TLS_CERT=/data/cert.pem
  TERMLYX_TLS_KEY=/data/key.pem`, your own certificate chain. The app checks it
  against the bundled public roots first and pins it if that fails. A pinned
  certificate renewed with a new key shows "certificate changed".
- **`off`**: plain HTTP, for a reverse proxy that terminates TLS. Binding a
  non-loopback address needs `TERMLYX_ALLOW_PLAIN=1`. Set `TERMLYX_URL` to the
  proxy's `https://` address.

A `self-signed` server behind a TLS proxy (clients see the proxy's certificate,
not the server's) also needs `TERMLYX_BEHIND_PROXY=1`, or setup and invite
short codes are refused: a client reaching the server directly always pins it.

Certificate files must be inside the data volume. The same settings can live in
`/data/server.toml` (keys `tls`, `tls_cert`, `tls_key`, `url`, `bind`, `names`,
`ips`, `allow_plain_remote`, `behind_proxy`); the environment wins.

## Storage limit

Each vault may store at most 256 MiB of sealed records, so one account cannot
fill the server's disk. A push that would grow a vault past the limit is
refused as a whole (`403 forbidden`, "this vault is full"); deleting items
always works. Change it with `TERMLYX_VAULT_QUOTA_MB` (or `vault_quota_mb` in
`/data/server.toml`); `0` turns the limit off.

## Backup and restore

A backup is a consistent copy of the database made while the server runs. It
holds password verifiers and wrapped keys but no plaintext; still keep it
private. For the image from `docs/self-host.md` (container `termlyx`, volume
`termlyx-data`):

    docker exec termlyx termlyx-server backup /data/backup-$(date +%F).db --with-tls
    docker cp termlyx:/data/backup-$(date +%F).db .
    docker cp termlyx:/data/backup-$(date +%F).db.tls.tar .

Without `--with-tls` the certificate is not saved; restoring on a fresh volume
would then create a new certificate with a new pin.

To restore, stop the container, copy the backup into the volume, make it owned
by the container's user (uid 10001; `docker cp` writes files as root and backups
are mode 0600), run `restore` as that user, remove the copies, start again.
`IMG` is the image the container runs:

    IMG=ghcr.io/clym252/termlyx-server:latest
    docker stop termlyx
    docker cp backup-2026-11-19.db termlyx:/data/restore.db
    docker cp backup-2026-11-19.db.tls.tar termlyx:/data/restore.db.tls.tar
    docker run --rm -u root -v termlyx-data:/data --entrypoint chown $IMG 10001:10001 /data/restore.db /data/restore.db.tls.tar
    docker run --rm -v termlyx-data:/data --entrypoint termlyx-server $IMG restore /data/restore.db --with-tls
    docker run --rm -u root -v termlyx-data:/data --entrypoint rm $IMG /data/restore.db /data/restore.db.tls.tar
    docker start termlyx

(Leave out the `.tls.tar` copy and `--with-tls` to keep the current
certificate.) The same recipe was verified earlier on Docker Desktop and a
Debian 13 VM with Compose and the `tether` service name.

`restore` validates the file, replaces the database atomically and writes a new
epoch, so every client notices the rewind and re-syncs instead of overwriting
newer data. It refuses to run while the server holds `/data/server.lock`.
(`--with-tls` expects `<file>.tls.tar` beside the database file.)

## Network and remote access

The server is for your LAN. **Do not port-forward it to the internet.** To sync
away from home, reach the host over a VPN (WireGuard, Tailscale).

## Admin CLI

Every subcommand reads the same `TERMLYX_*` settings as `serve` (`TERMLYX_DATA`
above all), so with Docker run them next to the running container:

    docker exec termlyx termlyx-server users list

| Command | What it does |
| --- | --- |
| `setup-code` | Reprint the first-account setup code (only while no account exists). |
| `invite [--note TEXT] [--expires 7d]` | Print a single-use short invite code (and the full `tether1-` code when `TERMLYX_URL` is set). |
| `link USER [--url ADDRESS]` | Print a 10 minute add-device code for a lone device that lost its session. `--url` is needed when `TERMLYX_URL` is not set. |
| `users list` | Username, created, disabled, number of devices. |
| `users disable USER` / `users enable USER` | Block / unblock an account (`forbidden` on login and refresh; its tokens stop working). |
| `users delete USER --yes` | Delete a **disabled** account (run `users disable USER` first) and everything it owns: devices, sessions, add-device codes and the vault's records. The username can then be registered again with a new invite. Without `--yes` it only says what it would delete. Cannot be undone except by `restore`. |
| `users reset-schema USER` | Let clients older than the newest one that connected write again after a bad upgrade. |
| `devices list USER` | Device id, name, platform, last seen, revoked. |
| `devices revoke DEVICE_ID` | Cut a device off. |
| `backup PATH [--with-tls]` | Consistent copy of the database (SQLite `VACUUM INTO`), safe while serving. `--with-tls` also writes `PATH.tls.tar` with the key and certificate. Refuses to overwrite. Files are mode 0600. |
| `restore PATH [--with-tls]` | Replace the database with a backup. The server must be stopped. Validates the file, swaps it in atomically and writes a **new epoch**, so clients notice the rewind and resync. |
| `fingerprint` | The certificate pin and names. |

A running server holds `{TERMLYX_DATA}/server.lock`; `restore` refuses while it
is held (a leftover file with no live server does not block anything). Paths
for `backup` and `restore` are paths inside the container when you use
`docker exec`: write into a mounted volume. A backup holds no
plaintext, but it does hold password verifiers and wrapped keys, so keep it
private.
