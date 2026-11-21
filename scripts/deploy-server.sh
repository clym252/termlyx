#!/usr/bin/env bash
# Build and (re)start tether-server on a Docker host, building ON that host.
#
# Usage: scripts/deploy-server.sh user@host [--dry-run]
#   user@host   required (the Docker host to build on)
#   --dry-run   stage the source tree locally, list what would be sent and the
#               commands that would run; touch nothing remote (no ssh at all)
#
# Environment (optional; also kept on the host in ~/tether/deploy.env so an
# upgrade without them keeps the earlier values):
#   TERMLYX_NAMES, TERMLYX_IPS, TERMLYX_URL, TERMLYX_PORT, TERMLYX_TLS,
#   TERMLYX_TLS_CERT, TERMLYX_TLS_KEY, TERMLYX_ALLOW_PLAIN, TERMLYX_BIND
#   (each also as TETHER_X; see server/README.md)
#   A name is required: TERMLYX_NAMES or TERMLYX_URL (or TETHER_NAMES/URL) must
#   be set now or already saved in ~/tether/deploy.env, else nothing is sent.
#
# What it does:
#   1. Stages a trimmed source tree in a temp directory: server/, crates/core-proto/
#      (the only workspace crate in the server's normal dependency graph),
#      server/deploy/Cargo.toml as the root Cargo.toml, and the root Cargo.lock
#      pruned to those crates. Not sent: apps/, vendor/, other crates, tests,
#      fixtures, keys.
#      Dev-dependencies: cargo must resolve them even though a release build
#      never compiles them (core-crypto, core-model, ... would have to be sent
#      too, with their own dependencies). Instead the staged manifests have
#      their [dev-dependencies] removed, so the tarball stays at the server and
#      core-proto; the Dockerfile builds with --locked on the pruned lock.
#   2. Prunes Cargo.lock by running `cargo metadata` on the stage, then checks
#      that nothing was added or changed against the root lock (only removals).
#   3. Over ssh: checks Docker Compose, warns on low memory (it never changes
#      the host), sends the tree (tar | ssh), `docker compose up -d --build`,
#      waits up to 120 s for health, prints the certificate fingerprint and how
#      to make the first invite.
#
# Needs locally: bash, tar, ssh, cargo (to prune the lock). Remotely: Docker
# with Compose v2+, tar.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"

HOST=""
DRY_RUN=0
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        -h | --help) sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
        *) HOST="$arg" ;;
    esac
done
[ -n "$HOST" ] || { echo "usage: scripts/deploy-server.sh user@host [--dry-run]" >&2; exit 2; }

log() { printf '==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# Remote locations (expanded by the remote shell).
# shellcheck disable=SC2016 # $HOME must expand on the host, not here
REMOTE_BASE='$HOME/tether'
REMOTE_SRC="$REMOTE_BASE/src"
REMOTE_ENV="$REMOTE_BASE/deploy.env"
COMPOSE_FILE='server/compose.yml'
# Compose invocation on the host: the env file is optional.
REMOTE_COMPOSE="cd $REMOTE_SRC && docker compose \$([ -f $REMOTE_ENV ] && echo --env-file $REMOTE_ENV) -f $COMPOSE_FILE"

# The first-invite command exactly as the user pastes it. The remote part is
# single-quoted so nothing ($HOME, the env-file test) expands locally, and
# `ssh -t` gives `docker compose exec` the terminal it wants.
INVITE_REMOTE="$REMOTE_COMPOSE exec tether termlyx-server invite"
INVITE_LINE="ssh -t $HOST '$INVITE_REMOTE'"
# Self-check: parse the printed line the way the user's shell will (ssh swapped
# for printf) and require the remote command to arrive byte-for-byte unexpanded.
# shellcheck disable=SC2294 # eval is the point: it mimics pasting the line
if [ "$(eval "printf '%s\\n' ${INVITE_LINE#ssh }" | tail -n 1)" != "$INVITE_REMOTE" ]; then
    die "internal error: the invite command would expand locally: $INVITE_LINE"
fi

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/tether-deploy.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

# --- 1. stage the trimmed tree ------------------------------------------------
# Deliberately a short allow-list: nothing else is copied.
log "staging the server source tree"
mkdir -p "$STAGE/crates"
cp -R "$ROOT/server" "$STAGE/server"
cp -R "$ROOT/crates/core-proto" "$STAGE/crates/core-proto"
rm -rf "$STAGE/server/tests" "$STAGE/server/target" \
    "$STAGE/crates/core-proto/tests" "$STAGE/crates/core-proto/target"
find "$STAGE" -name .DS_Store -delete
cp "$ROOT/server/deploy/Cargo.toml" "$STAGE/Cargo.toml"
cp "$ROOT/Cargo.lock" "$STAGE/Cargo.lock"
sh "$STAGE/server/deploy/strip-dev-deps.sh" "$STAGE/server/Cargo.toml" "$STAGE/crates/core-proto/Cargo.toml"

# The staged workspace must hold the server's whole normal dependency graph;
# a new workspace dependency of tether-server would otherwise fail on the host.
if (cd "$ROOT" && cargo tree -p tether-server -e normal --prefix none --offline 2>/dev/null) \
    | grep -E '\(/' | grep -vE '^(tether-server|core-proto) ' | grep -q .; then
    die "tether-server depends on a workspace crate besides core-proto; add it to this script and to server/deploy/Cargo.toml"
fi

# --- 2. prune the lock, verify no version moved --------------------------------
# One line per locked package: name, version, source, checksum.
lock_packages() {
    awk '
        function flush() { if (name != "") print name " " version " " source " " checksum }
        /^\[\[package\]\]/ { flush(); name = version = source = checksum = ""; next }
        /^name = /     { name = $3 }
        /^version = /  { version = $3 }
        /^source = /   { source = $3 }
        /^checksum = / { checksum = $3 }
        END { flush() }
    ' "$1" | sort -u
}

log "pruning Cargo.lock (cargo metadata)"
cargo metadata --manifest-path "$STAGE/Cargo.toml" --format-version 1 >/dev/null \
    || die "cargo metadata failed on the staged tree (network needed for crate downloads)"
lock_packages "$ROOT/Cargo.lock" >"$STAGE/.root-lock.txt"
lock_packages "$STAGE/Cargo.lock" >"$STAGE/.staged-lock.txt"
# Lines only in the staged lock = a package that was added or changed.
CHANGED="$(comm -13 "$STAGE/.root-lock.txt" "$STAGE/.staged-lock.txt")"
rm -f "$STAGE/.root-lock.txt" "$STAGE/.staged-lock.txt"
if [ -n "$CHANGED" ]; then
    printf '%s\n' "$CHANGED" >&2
    die "the staged Cargo.lock gained or changed packages versus the root Cargo.lock (listed above); fix server/deploy/Cargo.toml so it matches the root workspace dependencies"
fi
log "lock pruned: $(grep -c '^\[\[package\]\]' "$STAGE/Cargo.lock") of $(grep -c '^\[\[package\]\]' "$ROOT/Cargo.lock") packages, no version changes"

# GNU tar on the host warns about macOS extended attributes.
TAR_OPTS=()
if tar --version 2>/dev/null | grep -q bsdtar; then
    export COPYFILE_DISABLE=1
    TAR_OPTS+=(--no-xattrs)
fi
ARCHIVE="$STAGE.tgz"
trap 'rm -rf "$STAGE" "$ARCHIVE"' EXIT
tar "${TAR_OPTS[@]}" -czf "$ARCHIVE" -C "$STAGE" .

# --- optional settings kept on the host ---------------------------------------
ENV_LINES=""
for prefix in TERMLYX TETHER; do
    for suffix in NAMES IPS URL PORT TLS TLS_CERT TLS_KEY ALLOW_PLAIN BIND; do
        var="${prefix}_$suffix"
        if [ -n "${!var:-}" ]; then
            ENV_LINES+="$var=${!var}"$'\n'
        fi
    done
done
# Without a name the server has no public URL and codes would not say where it is.
NAME_SET=0
for var in TERMLYX_NAMES TERMLYX_URL TETHER_NAMES TETHER_URL; do
    [ -n "${!var:-}" ] && NAME_SET=1
done
NAME_MSG="set TERMLYX_NAMES=<the name your devices use for this server> (or TERMLYX_URL) for this deploy; it is saved in ~/tether/deploy.env on the host"

if [ "$DRY_RUN" = 1 ]; then
    log "dry run: the tarball would contain"
    tar -tzf "$ARCHIVE" | sed 's#^\./##' | grep -v '/$' | grep -v '^\.$' | sort
    echo
    log "total: $(tar -tzf "$ARCHIVE" | grep -vc '/$') files, $(du -h "$ARCHIVE" | cut -f1) compressed"
    echo
    log "commands that would run (nothing is run on a dry run)"
    if [ "$NAME_SET" = 1 ]; then
        log "server name: set in this environment"
    else
        log "server name: not set here; a real deploy stops unless $REMOTE_ENV on the host has a non-empty TERMLYX_NAMES, TERMLYX_URL, TETHER_NAMES or TETHER_URL"
    fi
    cat <<DRY
  ssh $HOST 'docker compose version --short; <sum of MemTotal + SwapTotal from /proc/meminfo>; <is a name saved in $REMOTE_ENV>'
  tar -czf - <staged tree> | ssh $HOST 'rm -rf $REMOTE_SRC && mkdir -p $REMOTE_SRC && tar -xzf - -C $REMOTE_SRC'
DRY
    if [ -n "$ENV_LINES" ]; then
        echo "  ssh $HOST 'write $REMOTE_ENV:'"
        printf '%s' "$ENV_LINES" | sed 's/^/      /'
    fi
    cat <<DRY
  ssh $HOST '$REMOTE_COMPOSE up -d --build'
  ssh $HOST '$REMOTE_COMPOSE exec -T tether termlyx-server health'    (repeated up to 120 s)
  ssh $HOST '$REMOTE_COMPOSE exec -T tether termlyx-server fingerprint'
DRY
    echo
    log "first invite command printed after a deploy (quoting verified: nothing expands locally)"
    echo "$INVITE_LINE"
    exit 0
fi

# --- 3. the host ---------------------------------------------------------------
SSH=(ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST")

log "checking $HOST"
# shellcheck disable=SC2016 # the awk program and $2 are for the remote shell
if ! PROBE="$("${SSH[@]}" 'docker compose version --short && awk "/^(MemTotal|SwapTotal):/ { s += \$2 } END { print s }" /proc/meminfo && { grep -Eqs "^(TERMLYX|TETHER)_(NAMES|URL)=." '"$REMOTE_ENV"' && echo named || echo unnamed; }' 2>&1)"; then
    printf '%s\n' "$PROBE" >&2
    die "cannot reach $HOST over ssh, or 'docker compose' (v2+) is missing there"
fi
COMPOSE_VERSION="$(printf '%s\n' "$PROBE" | sed -n 1p)"
MEM_KB="$(printf '%s\n' "$PROBE" | sed -n 2p)"
if [ "$NAME_SET" != 1 ] && [ "$(printf '%s\n' "$PROBE" | sed -n 3p)" != named ]; then
    die "$NAME_MSG"
fi
log "docker compose $COMPOSE_VERSION; memory + swap: $((MEM_KB / 1024)) MiB"
if [ "${MEM_KB:-0}" -lt $((3 * 1024 * 1024)) ]; then
    warn "$HOST has under 3 GiB of RAM + swap; the Rust build may be killed (out of memory)."
    warn "Give the host more RAM (4 GiB is comfortable); this script never changes the host."
fi

log "sending the source tree to $HOST:~/tether/src"
"${SSH[@]}" "rm -rf $REMOTE_SRC && mkdir -p $REMOTE_SRC"
"${SSH[@]}" "tar -xzf - -C $REMOTE_SRC" <"$ARCHIVE"

if [ -n "$ENV_LINES" ]; then
    log "saving settings to ~/tether/deploy.env"
    # Merge: variables given now replace earlier lines; the rest are kept.
    printf '%s' "$ENV_LINES" | "${SSH[@]}" "umask 077; mkdir -p $REMOTE_BASE; f=$REMOTE_ENV; cat > \"\$f.new\"; { awk -F= 'NR==FNR { k[\$1] = 1; next } !(\$1 in k)' \"\$f.new\" \"\$f\" 2>/dev/null || true; cat \"\$f.new\"; } > \"\$f.tmp\" && mv \"\$f.tmp\" \"\$f\"; rm -f \"\$f.new\""
fi

log "building and starting (the first build takes several minutes)"
"${SSH[@]}" "$REMOTE_COMPOSE up -d --build"

log "waiting for the server to become healthy (up to 120 s)"
healthy=0
deadline=$((SECONDS + 120))
while [ "$SECONDS" -lt "$deadline" ]; do
    if "${SSH[@]}" "$REMOTE_COMPOSE exec -T tether termlyx-server health" >/dev/null 2>&1; then
        healthy=1
        break
    fi
    sleep 3
done
if [ "$healthy" != 1 ]; then
    "${SSH[@]}" "$REMOTE_COMPOSE logs --tail 40 tether" >&2 || true
    die "the server did not become healthy within 120 s (logs above)"
fi
log "healthy"

echo
log "certificate (put this pin in the app if it asks for it; invites already carry it)"
"${SSH[@]}" "$REMOTE_COMPOSE exec -T tether termlyx-server fingerprint"
echo
log "first invite (paste this; the code is single-use, valid 7 days):"
echo "$INVITE_LINE"
