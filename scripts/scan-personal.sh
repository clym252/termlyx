#!/usr/bin/env bash
# Scan tracked files for private personal-info patterns (one ERE per line).
# Usage: scan-personal.sh [--patterns FILE] [PATH...]
# Exit: 0 clean, 1 matches (prints path:line only, never the text), 2 no pattern file.
set -u
pats="${TERMLYX_PERSONAL_PATTERNS:-}"
if [ "${1:-}" = "--patterns" ]; then pats="${2:-}"; shift 2 || true; fi
if [ -z "$pats" ] || [ ! -f "$pats" ]; then
  echo "personal pattern file missing (set TERMLYX_PERSONAL_PATTERNS or pass --patterns FILE)" >&2
  exit 2
fi
clean=$(mktemp); trap 'rm -f "$clean"' EXIT
grep -v '^[[:space:]]*$' "$pats" > "$clean"   # a blank line would match everything
[ -s "$clean" ] || { echo "personal pattern file missing (no patterns in it)" >&2; exit 2; }
# Fail closed on an invalid ERE (grep exits 2 with no output); never print the pattern.
grep -E -f "$clean" /dev/null 2>/dev/null; [ $? -eq 2 ] && { echo "personal pattern file invalid" >&2; exit 2; }
# -I skips binaries; stdout is cut to path:line so matched text never prints.
hits=$(git ls-files -z -- "$@" | xargs -0 -r grep -nHIEf "$clean" 2>/dev/null | cut -d: -f1,2)
[ -z "$hits" ] && exit 0
echo "$hits"; exit 1
