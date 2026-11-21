#!/usr/bin/env bash
# Usage: scripts/bump-version.sh X.Y.Z  (sets Cargo workspace, server/deploy/Cargo.toml, package.json, tauri.conf.json)
set -euo pipefail
[ $# -eq 1 ] || { echo "usage: $0 X.Y.Z" >&2; exit 2; }
exec node "$(dirname "$0")/release/version.mjs" bump "$1"
