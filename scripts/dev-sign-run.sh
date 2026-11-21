#!/usr/bin/env bash
# Cargo runner for `tauri dev` on macOS: optionally re-sign the freshly built
# debug binary with a stable Team-ID identity so Keychain "Always Allow"
# survives rebuilds. Opt-in via TETHER_DEV_SIGN_IDENTITY; see docs/dev-signing.md.
# This script never creates, imports or trusts a certificate.
# Usage: dev-sign-run.sh <binary> [args...]
set -u

if [ "$(uname -s)" = "Darwin" ] && [ -n "${TETHER_DEV_SIGN_IDENTITY:-}" ]; then
  if ! codesign --force --sign "$TETHER_DEV_SIGN_IDENTITY" \
    --identifier ca.clym.termlyx "$1" >/dev/null 2>&1; then
    echo "dev-sign: codesign failed; running unsigned (see docs/dev-signing.md)" >&2
  fi
fi

exec "$@"
