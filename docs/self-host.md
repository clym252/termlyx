# Self-host the Termlyx server

Sync between your devices needs a Termlyx server that you run. It stores
encrypted data only. This page gets one running and connects your first device.

What you need: a computer that stays on (a Linux box, a NAS, a spare PC, or a
Mac or Windows PC with Docker Desktop), on the same network as your devices.

## 1. Quick install

Docker must be installed and running first. Get it from
<https://docs.docker.com/engine/install/> (Linux),
<https://docs.docker.com/desktop/setup/install/mac-install/> (Mac) or
<https://docs.docker.com/desktop/setup/install/windows-install/> (Windows).
The scripts never use `sudo` and never change your system. If something is
missing they tell you and stop.

**Linux or macOS.** Open a terminal and run:

```sh
curl -fsSL https://github.com/clym252/termlyx/releases/latest/download/install.sh | sh
```

**Windows (Docker Desktop).** Open PowerShell and run:

```powershell
irm https://github.com/clym252/termlyx/releases/latest/download/install.ps1 | iex
```

The script downloads a small `compose.yml` into a `termlyx-server` folder in
your home folder, starts the server, waits until it is ready, and prints:

- a **setup code** (it looks like `TLX-7K2P-...`),
- the address or addresses you can reach it at, such as
  `https://192.168.1.20:8443`.

Keep that window open or copy both.

### In the app

1. Open Termlyx on the device you want to sync.
2. Open **Settings → Sync** and choose **Connect to server**.
3. In **Setup, invite or add-device code**, paste the setup code. Click
   **Continue**.
4. In **Server address**, type the address from the script, for example
   `192.168.1.20` or `nas.local`. You can leave out `https://`. If you type no
   port, the app uses 8443.
5. Choose a username and a master password, save your recovery key, and finish.

When the app says **Sync is on**, you are done. The setup code works once and
stops working as soon as the first account exists.

## 2. Manual install

Use this if you do not want a script, or you use a NAS. Pick one way only:
the script, Compose and `docker run` all make the same container `termlyx`.

### Docker Compose

1. Make a folder, for example `termlyx-server`, and put a file called
   `compose.yml` in it with exactly this:

   ```yaml
   services:
     termlyx:
       image: ghcr.io/clym252/termlyx-server:latest
       container_name: termlyx
       restart: unless-stopped
       ports: ["${TERMLYX_PORT:-8443}:8443"]
       volumes: [termlyx-data:/data]
   volumes:
     termlyx-data:
       name: termlyx-data
   ```

2. In that folder run:

   ```sh
   docker compose up -d
   ```

3. Show the setup code:

   ```sh
   docker logs termlyx
   ```

   In Docker Desktop you can instead open the container `termlyx` and read its
   **Logs** tab.
4. Follow "In the app" above. The server address is this computer's IP
   address: on Linux it is usually the first one `hostname -I` prints (not a
   Docker `172.17...` one), on a Mac see **System Settings → Network**, on
   Windows run `ipconfig` and use the **IPv4 Address** line.

### One `docker run` line

The same container as one command:

```sh
docker run -d --name termlyx --restart unless-stopped -p 8443:8443 -v termlyx-data:/data ghcr.io/clym252/termlyx-server:latest
```

Then `docker logs termlyx` for the setup code, and follow step 4 above.

### NAS (Synology Container Manager, Unraid, Portainer)

Open the screen where you add a project or stack, paste the `compose.yml` from
above, and start it. Then open the container's log to read the setup code. Do
not change the volume: your data lives in it.

## 3. Without Docker (Linux)

A plain binary is shipped for Linux only. On Mac and Windows, use Docker
Desktop (section 1).

1. Find your CPU type. `x86_64` is a normal PC; `aarch64` is 64-bit ARM:

   ```sh
   uname -m
   ```

2. Download the binary for it (use `aarch64` in place of `x86_64` if that is
   what `uname -m` printed) and the service file, then install them:

   ```sh
   ARCH=x86_64
   curl -fsSLO https://github.com/clym252/termlyx/releases/latest/download/termlyx-server-$ARCH-unknown-linux-musl
   curl -fsSLO https://github.com/clym252/termlyx/releases/latest/download/termlyx-server.service
   sudo install -m 755 termlyx-server-$ARCH-unknown-linux-musl /usr/local/bin/termlyx-server
   sudo useradd --system --no-create-home --shell /usr/sbin/nologin termlyx
   sudo cp termlyx-server.service /etc/systemd/system/
   sudo systemctl enable --now termlyx-server
   ```

3. Show the setup code:

   ```sh
   journalctl -u termlyx-server
   ```

Your data is in `/var/lib/termlyx`, owned by the `termlyx` user. The Docker
examples below use `docker exec termlyx termlyx-server <command>`; here run
admin commands as that user instead:

```sh
sudo -u termlyx TERMLYX_DATA=/var/lib/termlyx termlyx-server invite
sudo -u termlyx TERMLYX_DATA=/var/lib/termlyx termlyx-server setup-code
sudo -u termlyx TERMLYX_DATA=/var/lib/termlyx termlyx-server link USERNAME --url 192.168.1.20
sudo -u termlyx TERMLYX_DATA=/var/lib/termlyx termlyx-server backup /var/lib/termlyx/backup-2026-11-19.db --with-tls
```

To restore, stop the service first, hand the backup files to `termlyx`, and
restore as that user:

```sh
sudo systemctl stop termlyx-server
sudo install -o termlyx -g termlyx -m 600 backup-2026-11-19.db backup-2026-11-19.db.tls.tar /var/lib/termlyx/
sudo -u termlyx TERMLYX_DATA=/var/lib/termlyx termlyx-server restore /var/lib/termlyx/backup-2026-11-19.db --with-tls
sudo systemctl start termlyx-server
```

## 4. Add your other devices

On a device that already syncs:

1. Open **Settings → Sync** and choose **Add device**.
2. The screen shows an add-device code and a QR code. It works once and for 10
   minutes.
3. Under it, **Server address in this code** shows the address the new device
   will use. If the new device reaches the server by another name (for
   example a VPN address), click **Edit**, type it in **Server address**, and a
   new code is made.

On the new device: on first run choose **Join a synced vault** (or open
**Settings → Sync → Connect to server**), paste the code or scan the QR code,
and enter your username and master password.

If you have only one device and it lost its session, make a code on the server
instead (replace `USERNAME` and the address):

```sh
docker exec termlyx termlyx-server link USERNAME --url 192.168.1.20
```

## 5. Add another person

Each person gets a separate account. Nothing is shared between accounts.

```sh
docker exec termlyx termlyx-server invite
```

It prints a short code. Give it to the person with the server address. They
open **Settings → Sync → Connect to server** and use it like the setup code.
Options: `--note "for Sam"` to remember who it is for, and `--expires 12h`
(default `7d`; units `m`, `h`, `d`). Each code works once.

## 6. Update

Run the same install command again (section 1). It downloads the newest
`compose.yml`, pulls the new image and restarts the server. Your data and
accounts are not touched.

By hand, in the `termlyx-server` folder:

```sh
docker compose pull
docker compose up -d
```

**Remove the server.** In the `termlyx-server` folder, `docker compose down`
stops and removes the container and keeps your data. `docker compose down -v`
also deletes the data volume: every account and its data, for good. Back up
first (section 7). For the `docker run` way: `docker rm -f termlyx`, then
`docker volume rm termlyx-data` to delete the data.

## Verify your download

Every release file is listed in `SHA256SUMS`, has a build-provenance
attestation, and the server image is signed. Checking is optional but takes a
minute. Replace `X.Y.Z` with the version.

Checksums (download `SHA256SUMS` and the file into one folder):

```sh
sha256sum --check --ignore-missing SHA256SUMS      # Linux
shasum -a 256 --check --ignore-missing SHA256SUMS  # macOS
```

On Windows: `Get-FileHash <file> -Algorithm SHA256`, and compare with the
line in `SHA256SUMS`.

Attestation (needs the [GitHub CLI](https://cli.github.com/)):

```sh
gh attestation verify <file> --repo clym252/termlyx
```

Server image signature (needs [cosign](https://docs.sigstore.dev/cosign/)):

```sh
cosign verify ghcr.io/clym252/termlyx-server:vX.Y.Z \
  --certificate-identity-regexp '^https://github.com/clym252/termlyx/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

## 7. Backup and restore

A backup is a copy of the database. It holds no plain text, but keep it
private. `--with-tls` also saves the server's certificate key, so restoring
does not change the server's identity.

**Back up** (any day, while the server runs):

```sh
docker exec termlyx termlyx-server backup /data/backup-2026-11-19.db --with-tls
docker cp termlyx:/data/backup-2026-11-19.db .
docker cp termlyx:/data/backup-2026-11-19.db.tls.tar .
```

Use today's date in place of `2026-11-19`. Without `--with-tls`, skip the
last line.

**Restore** replaces everything with the backup. The server must be stopped.
The container's user has id 10001, so the files you copy in must be handed to
it. Run these in the folder holding the backup files:

```sh
docker stop termlyx
docker cp backup-2026-11-19.db termlyx:/data/restore.db
docker cp backup-2026-11-19.db.tls.tar termlyx:/data/restore.db.tls.tar
docker run --rm -u root -v termlyx-data:/data --entrypoint chown ghcr.io/clym252/termlyx-server:latest 10001:10001 /data/restore.db /data/restore.db.tls.tar
docker run --rm -v termlyx-data:/data --entrypoint termlyx-server ghcr.io/clym252/termlyx-server:latest restore /data/restore.db --with-tls
docker run --rm -u root -v termlyx-data:/data --entrypoint rm ghcr.io/clym252/termlyx-server:latest /data/restore.db /data/restore.db.tls.tar
docker start termlyx
```

Leave out the `.tls.tar` lines and `--with-tls` to keep the current
certificate. Devices notice the restore and sync again from the restored data.

## 8. Away from home

The server is built for your home network. **Do not port-forward it to the
internet.** To sync away from home, put the server and your devices on a VPN
such as Tailscale or WireGuard, and use the server's VPN address in the app.
Sync is end-to-end encrypted, but an open port still invites guessing and
probing.

## 9. Advanced

### Own certificate

By default the server makes its own certificate and the app remembers it
(pins it) the first time you connect. To use your own certificate instead, put
the files inside the data volume, then set the mode.

1. Copy the files in (container running):

   ```sh
   docker cp cert.pem termlyx:/data/cert.pem
   docker cp key.pem termlyx:/data/key.pem
   ```

2. Add this under `termlyx:` in `compose.yml`, then run
   `docker compose up -d`:

   ```yaml
       environment:
         TERMLYX_TLS: files
         TERMLYX_TLS_CERT: /data/cert.pem
         TERMLYX_TLS_KEY: /data/key.pem
   ```

If the certificate comes from a public authority, the app checks it normally.
If not, the app pins it. The app trusts only the public authorities it ships
with (the Mozilla list), never the certificates installed on your devices, so
a private CA counts as "not public". A pinned certificate that is later renewed
**with a new key** shows "This server's certificate changed since you
connected." Fix it by keeping the same key when you renew.

### Behind a reverse proxy

Let the proxy handle TLS and run the server without it:

```yaml
    environment:
      TERMLYX_TLS: "off"
      TERMLYX_ALLOW_PLAIN: "1"
      TERMLYX_URL: https://termlyx.example.com
```

`TERMLYX_ALLOW_PLAIN` is needed because the server listens on a non-loopback
address. Point the proxy at port 8443 of the container.

The proxy's certificate must be **publicly trusted** for the name in
`TERMLYX_URL`: the app checks it against the public authorities it ships with
and never against your devices' own trust store, and with TLS off there is no
server certificate to pin. Use, for example, Let's Encrypt with a DNS
challenge (works for a name that only resolves on your network) or Tailscale
serve. A certificate from your own CA or a self-signed proxy certificate does
not work.

### Testing overrides

The install scripts read `TERMLYX_RELEASE_BASE` (where `compose.yml` is
downloaded from) and `TERMLYX_IMAGE` (a local or mirrored image; the script
then skips the pull). They are for testing, not normal installs. Set them on
the `sh` side of the pipe, for example
`curl -fsSL "$BASE/install.sh" | TERMLYX_RELEASE_BASE="$BASE" TERMLYX_IMAGE=termlyx-server:test sh`;
in PowerShell set `$env:TERMLYX_RELEASE_BASE` and `$env:TERMLYX_IMAGE` first.
`TERMLYX_PORT` (the host port, default 8443) works the same way and is
remembered for later runs.

More settings and the full admin command list are in
[`server/README.md`](../server/README.md).

## 10. Troubleshooting

The app shows these messages. Find yours.

**Can't reach 192.168.1.20:8443. Is the server running, and is this device on the same network?**
Run `docker ps` on the server and check `termlyx` is `Up` and healthy. Check
the address, and that the device is on the same network or VPN. Check the
server computer's firewall allows port 8443.

**That address answers, but it is not a Termlyx server. Check the address and
port.**
Something else answers there. Check the address and the port (8443 unless you
changed it).

**That code is not valid on this server. Get the setup code from the server's
log, or a new invite.**
The code is wrong, used, or expired. Show the setup code again (it only
exists until the first account is made):

```sh
docker exec termlyx termlyx-server setup-code
```

For a second person, make an invite (section 5).

**Couldn't verify this server. Make sure the address is your Termlyx server,
not another device.**
The server at that address does not know your code. Check the address. Do not
continue if the address is right and you did not change anything: someone may
be intercepting the connection.

**The app shows a fingerprint and asks you to compare it.**
Print the server's fingerprint and check that the two match:

```sh
docker exec termlyx termlyx-server fingerprint
```

**This server's certificate changed since you connected. If you reinstalled the
server, connect again with a new code.**
Did you reinstall the server, lose its volume, or renew a pinned certificate
with a new key? If so, turn off sync on the device and connect again with a
new code (section 4 or 5). If not, do not continue.

**That server address isn't valid. Use an address like 192.168.1.20, nas.local
or nas.local:8443.**
Type only the address, with an optional port. No path.

**Port 8443 is already in use** (the server does not start, or another app
answers on 8443; a UniFi Network app uses it, for example).
Map another host port, for example 9443:

- Install script: run the install command again with `TERMLYX_PORT=9443` on the
  `sh` side of the pipe (`curl ... | TERMLYX_PORT=9443 sh`; PowerShell:
  `$env:TERMLYX_PORT=9443; irm ... | iex`). It is remembered for later updates.
- Compose by hand: put `TERMLYX_PORT=9443` in a file named `.env` next to
  `compose.yml`, then run `docker compose up -d`.
- `docker run`: change `-p 8443:8443` to `-p 9443:8443`.

Either way, type the address with that port in the app, for example
`192.168.1.20:9443`.

To see what the server is doing: `docker logs termlyx`.
