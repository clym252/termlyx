# Sourced by the helper scripts: loads per-machine settings (hosts, keys, Team ID) from
# ${TERMLYX_LOCAL_CONFIG:-~/.config/termlyx/local.sh} when it exists. Variables already set in the
# environment win over the file. Names: TETHER_DEVVM_HOST, TETHER_WIN_HOST, TETHER_DEVVM_KEY,
# TETHER_WIN_KEY, CARGO_TARGET_DIR, APPLE_DEVELOPMENT_TEAM.
_tlc="${TERMLYX_LOCAL_CONFIG:-$HOME/.config/termlyx/local.sh}"
if [[ -f "$_tlc" ]]; then
  _tlc_saved="$(export -p)"
  . "$_tlc"
  eval "$_tlc_saved" 2>/dev/null || true # re-apply explicit env over the file's values
  unset _tlc_saved
fi
unset _tlc
