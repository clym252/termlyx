#!/usr/bin/env bash
# Renders one clip on the devvm and copies the web encodes into website/src/media/.
#   website/clips/render.sh <clip>        e.g. hero
# Renders what is committed (devvm.sh sync sends HEAD): commit the composition first.
# Needs TETHER_DEVVM_HOST (and optionally TETHER_DEVVM_KEY) from scripts/local-config.sh.
set -euo pipefail

clip="${1:?usage: website/clips/render.sh <clip>}"
[[ "$clip" =~ ^[a-z0-9-]+$ ]] || { echo "bad clip name: $clip" >&2; exit 2; }
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
[[ -f "website/clips/$clip/index.html" ]] || { echo "no composition: website/clips/$clip/index.html" >&2; exit 2; }
source scripts/local-config.sh
: "${TETHER_DEVVM_HOST:?set TETHER_DEVVM_HOST (scripts/local-config.sh)}"

KEY_OPT=()
if [[ -n "${TETHER_DEVVM_KEY:-}" && -f "$TETHER_DEVVM_KEY" ]]; then KEY_OPT=(-i "$TETHER_DEVVM_KEY"); fi
SSH=(ssh -o BatchMode=yes -o ConnectTimeout=10 ${KEY_OPT[@]+"${KEY_OPT[@]}"})
SCP=(scp -q -o BatchMode=yes ${KEY_OPT[@]+"${KEY_OPT[@]}"})

render() {
  scripts/devvm.sh sync

  "${SSH[@]}" "$TETHER_DEVVM_HOST" bash -s -- "$clip" <<'REMOTE'
set -euo pipefail
clip="$1"
# This script arrives on stdin: the block is parsed whole, then runs with stdin closed.
{
export HYPERFRAMES_NO_TELEMETRY=1 DO_NOT_TRACK=1 HYPERFRAMES_NO_UPDATE_CHECK=1 HYPERFRAMES_NO_AUTO_INSTALL=1
cd ~/src/termlyx/website/clips
npm ci --no-audit --no-fund --loglevel=error
npx hyperframes lint "$clip"
rm -rf renders media && mkdir -p renders media
# A near-lossless master (the web encodes below are cut from it). shared/ and gsap/ are
# symlinks out of the clip folder: one copy of the app styles for every clip.
npx hyperframes render "$clip" --include-outside-files --fps 30 --crf 10 -o "renders/$clip.mp4"
ffmpeg -loglevel error -y -i "renders/$clip.mp4" -an -c:v libx264 -crf 28 -preset slow -pix_fmt yuv420p -movflags +faststart "media/$clip.mp4"
ffmpeg -loglevel error -y -i "renders/$clip.mp4" -an -c:v libvpx-vp9 -crf 40 -b:v 0 -row-mt 1 -pix_fmt yuv420p "media/$clip.webm"
ffmpeg -loglevel error -y -ss 0 -i "renders/$clip.mp4" -frames:v 1 -q:v 4 "media/$clip.jpg"
ls -l media
} </dev/null
REMOTE

  mkdir -p website/src/media
  for ext in mp4 webm jpg; do
    "${SCP[@]}" "$TETHER_DEVVM_HOST:src/termlyx/website/clips/media/$clip.$ext" "website/src/media/$clip.$ext"
  done
  ls -l website/src/media/"$clip".*
}

# The devvm disk is nearly full: keep nothing there but the checkout and node_modules, even on
# failure. Set here, not in render: the pipeline runs render in a subshell that may skip its traps.
trap '"${SSH[@]}" "$TETHER_DEVVM_HOST" "rm -rf ~/src/termlyx/website/clips/renders ~/src/termlyx/website/clips/media" 2>/dev/null || true' EXIT

# Logs never name the devvm or its user. A plain pipe (not process substitution), so every
# line is through the filter before the script exits; the exit status is render's.
user_part="${TETHER_DEVVM_HOST%@*}"
host_part="${TETHER_DEVVM_HOST#*@}"
render 2>&1 | DEVVM_HOST="$TETHER_DEVVM_HOST" DEVVM_NAME="$host_part" DEVVM_USER="$user_part" perl -pe '
  BEGIN { $| = 1 }
  s/\Q$ENV{DEVVM_HOST}\E/devvm/g; s/\Q$ENV{DEVVM_NAME}\E/devvm/g;
  s/\b\Q$ENV{DEVVM_USER}\E\b/user/g if $ENV{DEVVM_USER} ne $ENV{DEVVM_HOST};'
