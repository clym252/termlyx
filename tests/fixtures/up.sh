#!/usr/bin/env bash
# Start the Tether SSH test fixture and wait until sshd is ready for auth.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOST=127.0.0.1
PORT=2222
TIMEOUT=30
CONTAINER=tether-sshd

chmod 600 "$DIR/keys/id_ed25519_test"

# Docker is devvm's through scripts/devvm.sh (DOCKER_HOST) or the Mac's own.
if ! docker info >/dev/null 2>&1; then
  echo "error: no Docker daemon answers (DOCKER_HOST=${DOCKER_HOST:-unset})." >&2
  echo "  devvm: eval \"\$(scripts/devvm.sh up)\"   or start Docker Desktop" >&2
  exit 1
fi

# --build: the sshd image is the pinned linuxserver one plus tmux (Dockerfile);
# cached after the first build.
docker compose -f "$DIR/docker-compose.yml" up -d --build

deadline=$((SECONDS + TIMEOUT))

# Phase 1: TCP port accepts connections.
until nc -z "$HOST" "$PORT" >/dev/null 2>&1; do
  if ((SECONDS >= deadline)); then
    echo "error: sshd fixture did not open $HOST:$PORT within ${TIMEOUT}s" >&2
    docker compose -f "$DIR/docker-compose.yml" logs --tail 30 sshd >&2 || true
    exit 1
  fi
  sleep 1
done

# Phase 2: sshd actually answers with a host key (TCP may accept earlier).
until [ -n "$(ssh-keyscan -T 2 -p "$PORT" "$HOST" 2>/dev/null)" ]; do
  if ((SECONDS >= deadline)); then
    echo "error: $HOST:$PORT is open but sshd returned no host key within ${TIMEOUT}s" >&2
    docker compose -f "$DIR/docker-compose.yml" logs --tail 30 sshd >&2 || true
    exit 1
  fi
  sleep 1
done

# Phase 3: port forwarding is enabled (sshd-init/) and the forward target
# (web:80, the whoami service) answers from inside the sshd container.
for line in 'AllowTcpForwarding yes' 'GatewayPorts clientspecified'; do
  if ! docker exec "$CONTAINER" grep -qx "$line" /config/sshd/sshd_config; then
    echo "error: '$line' is not set in $CONTAINER (was it created from another compose file?)" >&2
    exit 1
  fi
done
until docker exec "$CONTAINER" curl -fsS -m 2 -o /dev/null http://web:80/ >/dev/null 2>&1; do
  if ((SECONDS >= deadline)); then
    echo "error: web:80 (whoami) is not reachable from $CONTAINER within ${TIMEOUT}s" >&2
    docker compose -f "$DIR/docker-compose.yml" logs --tail 30 web >&2 || true
    exit 1
  fi
  sleep 1
done

echo "sshd fixture ready at $HOST:$PORT (user: tether; forwarding on; web:80 up)"
