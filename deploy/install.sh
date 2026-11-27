#!/bin/sh
# Install or update the Termlyx server with Docker. No sudo. Re-run to update.
#   curl -fsSL https://github.com/clym252/termlyx/releases/latest/download/install.sh | sh
# Hidden overrides: TERMLYX_RELEASE_BASE (where compose.yml and SHA256SUMS come from),
# TERMLYX_IMAGE (a local or mirrored image; skips the pull).
# TERMLYX_PORT (host port, default 8443) is remembered for later runs.
set -eu

# All in main(), called on the last line: a download cut short by `curl | sh`
# defines nothing it runs.
main() {

OWNER="clym252" # GitHub owner; keep in step with compose.yml and release.yml
BASE="${TERMLYX_RELEASE_BASE:-https://github.com/$OWNER/termlyx/releases/latest/download}"
DIR="$HOME/termlyx-server"

die() { echo "error: $*" >&2; exit 1; }

# compose.yml pins the image by digest, so it must arrive untampered: https
# only, plain http just for a server on this machine.
case "$BASE" in
  https://*) ;;
  *@*) die "TERMLYX_RELEASE_BASE must be an https:// URL" ;;
  http://localhost | http://localhost[:/]* | http://127.0.0.1 | http://127.0.0.1[:/]* | "http://[::1]" | "http://[::1]"[:/]*) ;;
  *) die "TERMLYX_RELEASE_BASE must be an https:// URL (plain http only for localhost)" ;;
esac

case "$(uname -s)" in
  Darwin) HELP="https://docs.docker.com/desktop/setup/install/mac-install/" ;;
  *) HELP="https://docs.docker.com/engine/install/" ;;
esac
command -v docker >/dev/null 2>&1 || die "Docker is not installed. Install it from $HELP and run this again."
docker compose version >/dev/null 2>&1 || die "Docker Compose v2 is missing. See $HELP and run this again."
docker info >/dev/null 2>&1 || die "Cannot talk to Docker. Start it, or add your user to the docker group (this script never uses sudo)."

mkdir -p "$DIR"
cd "$DIR"
# Compose reads this file next to compose.yml. It carries TERMLYX_IMAGE and
# TERMLYX_PORT; the port from the environment wins, else the saved one stays.
VARS=".env"
PORT="${TERMLYX_PORT:-}"
[ -n "$PORT" ] || PORT="$(sed -n 's/^TERMLYX_PORT=//p' "$VARS" 2>/dev/null | tail -n 1)"
if [ -n "$PORT" ]; then
  case "$PORT" in
    [1-9] | [1-9][0-9] | [1-9][0-9][0-9] | [1-9][0-9][0-9][0-9] | [1-9][0-9][0-9][0-9][0-9]) [ "$PORT" -le 65535 ] || die "TERMLYX_PORT must be a number from 1 to 65535" ;;
    *) die "TERMLYX_PORT must be a number from 1 to 65535" ;;
  esac
fi
# Download beside the old file, replace it only on success and a checksum match.
fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$BASE/$1" -o "$1.tmp" || { rm -f compose.yml.tmp SHA256SUMS.tmp; die "could not download $BASE/$1"; }
  else
    wget -qO "$1.tmp" "$BASE/$1" || { rm -f compose.yml.tmp SHA256SUMS.tmp; die "could not download $BASE/$1"; }
  fi
}
fetch compose.yml
fetch SHA256SUMS
if command -v sha256sum >/dev/null 2>&1; then got="$(sha256sum compose.yml.tmp)"; else got="$(shasum -a 256 compose.yml.tmp)"; fi
got="${got%% *}"
# sha256sum writes "<hex>  <name>", or "<hex> *<name>" in binary mode.
want="$(awk '$2 == "compose.yml" || $2 == "*compose.yml" { print $1; exit }' SHA256SUMS.tmp)"
rm -f SHA256SUMS.tmp
[ -n "$want" ] && [ "$got" = "$want" ] || { rm -f compose.yml.tmp; die "compose.yml did not match the release checksum"; }
mv compose.yml.tmp compose.yml
{
  [ -z "${TERMLYX_IMAGE:-}" ] || echo "TERMLYX_IMAGE=$TERMLYX_IMAGE"
  [ -z "$PORT" ] || echo "TERMLYX_PORT=$PORT"
} >"$VARS"
[ -s "$VARS" ] || rm -f "$VARS"
[ -n "${TERMLYX_IMAGE:-}" ] || docker compose pull
docker compose up -d

echo "Waiting for the server to start (up to 2 minutes)..."
i=0
status=""
while [ "$i" -lt 60 ]; do
  status="$(docker inspect -f '{{.State.Health.Status}}' termlyx 2>/dev/null || true)"
  [ "$status" = healthy ] && break
  i=$((i + 1))
  sleep 2
done
[ "$status" = healthy ] || { docker logs --tail 40 termlyx >&2 || true; die "the server did not become healthy in 2 minutes (log above)"; }

echo
# Status 3 means the server already has an account: an update, not a failure.
# stdout (the banner) goes to the terminal through fd 3; stderr is captured.
exec 3>&1
rc=0
err="$(docker exec termlyx termlyx-server setup-code 2>&1 >&3)" || rc=$?
exec 3>&-
case "$rc" in
  0) ;;
  3) echo "Server updated." ;;
  *) printf '%s\n' "$err" >&2; die "could not read the setup code (exit status $rc, error above)" ;;
esac

echo
echo "Reach it from the Termlyx app at one of:"
case "$(uname -s)" in
  Darwin) ips="$(ipconfig getifaddr en0 2>/dev/null || true; ipconfig getifaddr en1 2>/dev/null || true)" ;;
  *)
    if command -v ip >/dev/null 2>&1; then
      # Skip Docker's own bridges.
      ips="$(ip -o -4 addr show scope global 2>/dev/null | awk '$2 !~ /^(docker0|br-|veth)/ { split($4, a, "/"); print a[1] }')"
    else
      ips="$(hostname -I 2>/dev/null | tr ' ' '\n')"
    fi ;;
esac
for ip in $ips; do
  case "$ip" in *:* | 169.254.*) ;; *) echo "  https://$ip:${PORT:-8443}" ;; esac # IPv4 only
done
}

main "$@"
