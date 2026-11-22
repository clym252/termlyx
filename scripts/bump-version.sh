#!/usr/bin/env bash
# Usage: scripts/bump-version.sh X.Y.Z  (sets Cargo workspace, server/deploy/Cargo.toml, package.json, tauri.conf.json, Cargo.lock)
set -euo pipefail
[ $# -eq 1 ] || { echo "usage: $0 X.Y.Z" >&2; exit 2; }
cd "$(dirname "$0")/.."
node scripts/release/version.mjs bump "$1"
# Workspace crate versions live in Cargo.lock too; --locked builds need it current.
cargo update --workspace --offline --quiet
