// Fails on any production npm dependency whose license is outside the allow list.
// Keep ALLOWED_LICENSES identical to [licenses] allow in deny.toml.
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const ALLOWED_LICENSES = [
  "MIT",
  "Apache-2.0",
  "Apache-2.0 WITH LLVM-exception",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "ISC",
  "Zlib",
  "Unicode-3.0",
  "CC0-1.0",
  "BSL-1.0",
  "MPL-2.0",
  "OFL-1.1",
  "CDLA-Permissive-2.0",
];

// Fails closed: one flat OR-list or AND-list, optionally wrapped in one paren pair.
// shortcut: no nested or mixed OR/AND (treated as a violation), upgrade if a dep needs it.
function ok(expr) {
  let e = expr.trim();
  if (e.startsWith("(") && e.endsWith(")") && !e.slice(1, -1).includes(")")) e = e.slice(1, -1).trim();
  if (/[()]/.test(e)) return false;
  const or = /\sOR\s/.test(e);
  const and = /\sAND\s/.test(e);
  if (or && and) return false;
  if (or) return e.split(/\s+OR\s+/).some(ok);
  if (and) return e.split(/\s+AND\s+/).every(ok);
  return ALLOWED_LICENSES.includes(e);
}

export function checkLicenses(entries) {
  return entries
    .filter((p) => typeof p.license !== "string" || !ok(p.license))
    .map((p) => ({ name: p.name, license: p.license }));
}

// pnpm prints { "<license>": [{ name, versions: [..] }] }.
export function flattenPnpm(byLicense) {
  return Object.entries(byLicense).flatMap(([license, pkgs]) =>
    pkgs.flatMap((p) => p.versions.map((version) => ({ name: p.name, version, license }))),
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const out = execFileSync("pnpm", ["-C", "apps/tauri", "licenses", "list", "--prod", "--json"], {
    encoding: "utf8",
    maxBuffer: 1 << 28,
  });
  const entries = flattenPnpm(JSON.parse(out));
  const bad = checkLicenses(entries);
  for (const b of bad) console.error(`disallowed license: ${b.name} (${b.license})`);
  console.log(`${entries.length} packages checked, ${bad.length} violations`);
  process.exit(bad.length ? 1 : 0);
}
