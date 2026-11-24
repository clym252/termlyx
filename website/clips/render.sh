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

# Logs never name the devvm.
host_part="${TETHER_DEVVM_HOST#*@}"
exec > >(perl -pe "s/\Q$TETHER_DEVVM_HOST\E/devvm/g; s/\Q$host_part\E/devvm/g") 2>&1

SSH=(ssh -o BatchMode=yes -o ConnectTimeout=10 ${TETHER_DEVVM_KEY:+-i "$TETHER_DEVVM_KEY"})
SCP=(scp -q -o BatchMode=yes ${TETHER_DEVVM_KEY:+-i "$TETHER_DEVVM_KEY"})

scripts/devvm.sh sync

# The devvm disk is nearly full: keep nothing there but the checkout and node_modules, even on failure.
cleanup() { "${SSH[@]}" "$TETHER_DEVVM_HOST" "rm -rf ~/src/termlyx/website/clips/renders ~/src/termlyx/website/clips/media" || true; }
trap cleanup EXIT

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
