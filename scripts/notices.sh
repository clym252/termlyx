#!/usr/bin/env bash
# Regenerates apps/tauri/src/about/THIRD-PARTY-NOTICES.txt (bundled into Settings → About).
# Needs cargo-about and pnpm with apps/tauri installed; runs on devvm. Output is
# deterministic, so CI re-runs it and fails on a diff.
set -euo pipefail
cd "$(dirname "$0")/.."
export LC_ALL=C
out=apps/tauri/src/about/THIRD-PARTY-NOTICES.txt
mkdir -p "$(dirname "$out")"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

{
  echo "THIRD-PARTY NOTICES"
  echo
  cargo about generate --locked --all-features about.hbs
  echo
  echo "================================================================================"
  echo "NPM DEPENDENCIES (production)"
  pnpm -C apps/tauri licenses list --prod --json | node -e '
    const fs = require("fs");
    const path = require("path");
    const rows = [];
    for (const [license, pkgs] of Object.entries(JSON.parse(fs.readFileSync(0, "utf8"))))
      for (const p of pkgs)
        p.versions.forEach((version, i) => rows.push({ name: p.name, version, license, dir: p.paths[i] }));
    rows.sort((a, b) => a.name.localeCompare(b.name, "en") || a.version.localeCompare(b.version, "en"));
    for (const r of rows) {
      const file = fs.readdirSync(r.dir).sort().find((f) => /^(licen[sc]e|copying)/i.test(f));
      const text = file ? fs.readFileSync(path.join(r.dir, file), "utf8").trimEnd() : "(no license file shipped)";
      console.log(`\n${r.name} ${r.version}\nLicense: ${r.license}\n\n${text}`);
    }
  '
  echo
  echo "================================================================================"
  echo "FONT: JetBrains Mono (OFL-1.1), bundled via @fontsource/jetbrains-mono"
  echo
  cat apps/tauri/node_modules/@fontsource/jetbrains-mono/LICENSE
  echo
  echo "================================================================================"
  echo "russh (Apache-2.0), vendored in vendor/russh with Termlyx patches"
  echo
  cat vendor/russh/LICENSE.txt
} >"$tmp"
mv "$tmp" "$out"
trap - EXIT
