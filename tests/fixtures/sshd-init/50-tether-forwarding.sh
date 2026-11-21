#!/usr/bin/with-contenv bash
# Tether test fixture: allow port forwarding (local, remote and dynamic),
# accept LANG and LC_* environment variables from clients, and turn off
# OpenSSH's PerSourcePenalties (sshd >= 9.8): the tests fail
# authentication on purpose (wrong and stale passwords, rejected host keys),
# all from one source address, and the penalties would make sshd drop the
# connections of later tests.
#
# The image's init writes /config/sshd/sshd_config with
# "AllowTcpForwarding no" and "GatewayPorts no". The image runs this script
# (copied to /custom-cont-init.d by the Dockerfile) after its own init and before sshd starts.
# Idempotent: it rewrites those lines in place, so they keep their position
# ahead of any Match block.
set -euo pipefail

CONF=/config/sshd/sshd_config
sed -i \
  -e 's/^AllowTcpForwarding .*/AllowTcpForwarding yes/' \
  -e 's/^GatewayPorts .*/GatewayPorts clientspecified/' \
  "$CONF"

# The file has no Match block, so a line appended at the end is global.
grep -qx 'PerSourcePenalties no' "$CONF" || echo 'PerSourcePenalties no' >>"$CONF"

# Per-channel `env` requests (a tab's id, LC_TETHER_TAB) need a matching
# AcceptEnv; the stock config has none.
grep -qx 'AcceptEnv LANG LC_\*' "$CONF" || echo 'AcceptEnv LANG LC_*' >>"$CONF"

if ! grep -qx 'AllowTcpForwarding yes' "$CONF" ||
  ! grep -qx 'GatewayPorts clientspecified' "$CONF" ||
  ! grep -qx 'AcceptEnv LANG LC_\*' "$CONF"; then
  echo "tether: could not enable TCP forwarding / AcceptEnv in $CONF" >&2
  exit 1
fi
echo "tether: TCP forwarding enabled (GatewayPorts clientspecified), source penalties off"
