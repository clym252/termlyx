#!/usr/bin/env bash
# Tests for scan-personal.sh. Synthetic patterns only; run in a temp git repo.
# The real pattern file must not contain the synthetic word used below, or this file self-matches.
set -u
S="$(cd "$(dirname "$0")" && pwd)/scan-personal.sh"
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT
fail=0
check() { if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1 (want $3 got $2)"; fail=1; fi; }

cd "$T" && git init -q . && echo "hello" > clean.txt && git add . 
printf 'secretword123\n' > pats

env -u TERMLYX_PERSONAL_PATTERNS "$S" >/dev/null 2>err; rc=$?
check "personal scan fails without patterns" "$rc" 2
grep -q "personal pattern file missing" err || { echo "FAIL message"; fail=1; }

"$S" --patterns pats >/dev/null; check "clean tree exits 0" "$?" 0

printf 'a\nb secretword123 c\n' > dirty.txt && git add dirty.txt
out=$("$S" --patterns pats); rc=$?
check "planted match exits 1" "$rc" 1
case "$out" in *dirty.txt:2*) echo "ok   output has path:line";; *) echo "FAIL output lacks path:line"; fail=1;; esac
case "$out" in *secretword123*) echo "FAIL output leaks match"; fail=1;; *) echo "ok   output hides match";; esac

printf '(\n' > bad; "$S" --patterns bad >/dev/null 2>&1; check "malformed pattern fails closed" "$?" 2

TERMLYX_PERSONAL_PATTERNS=pats "$S" >/dev/null; check "env var default" "$?" 1
exit $fail
