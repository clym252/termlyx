#!/usr/bin/env bash
# Stop and remove the Tether SSH test fixture.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
docker compose -f "$DIR/docker-compose.yml" down
