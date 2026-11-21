#!/bin/sh
# Remove [dev-dependencies] tables from the given Cargo.toml files, in place.
#
# The server build never compiles dev-dependencies, but cargo must still be
# able to resolve them (their path crates must exist, and they stay in
# Cargo.lock). Dropping them lets the trimmed tree carry only `server/` and
# `crates/core-proto/` and keeps the pruned lock file small. Used by the deploy
# script and, as a no-op safety net, by the Dockerfile.
set -eu
for f in "$@"; do
    awk '/^\[/ { skip = ($0 ~ /^\[(target\..*\.)?dev-dependencies(\..*)?\]/) } !skip' "$f" > "$f.tmp"
    mv "$f.tmp" "$f"
done
