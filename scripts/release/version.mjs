// One version for every platform: Cargo workspace, server deploy manifest, package.json, tauri.conf.json.
import { readFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const F = {
  cargo: "Cargo.toml",
  // Replaces the root manifest in server Docker builds, so it carries its own version.
  deploy: "server/deploy/Cargo.toml",
  npm: "apps/tauri/package.json",
  tauri: "apps/tauri/src-tauri/tauri.conf.json",
};
const SEMVER = /^\d+\.\d+\.\d+$/;
// Matches the `version = "…"` line inside the [workspace.package] section only.
const CARGO_RE = /(\[workspace\.package\][^[]*?\nversion\s*=\s*")([^"]*)(")/;

const read = (root, k) => readFileSync(join(root, F[k]), "utf8");

function cargoVersion(root, k) {
  const m = read(root, k).match(CARGO_RE);
  if (!m) throw new Error(`${F[k]}: no [workspace.package] version`);
  return m[2];
}

export function readVersions(root) {
  return {
    cargo: cargoVersion(root, "cargo"),
    deploy: cargoVersion(root, "deploy"),
    npm: JSON.parse(read(root, "npm")).version,
    tauri: JSON.parse(read(root, "tauri")).version,
  };
}

export function checkTag(tag, v) {
  const m = /^v(\d+\.\d+\.\d+)$/.exec(tag);
  if (!m) throw new Error(`tag must be vX.Y.Z, got "${tag}"`);
  const bad = Object.entries(F)
    .filter(([k]) => v[k] !== m[1])
    .map(([k, f]) => `${f} (${v[k]})`);
  if (bad.length) throw new Error(`tag ${tag} does not match: ${bad.join(", ")}`);
}

export function bump(root, version) {
  if (!SEMVER.test(version)) throw new Error(`version must be X.Y.Z, got "${version}"`);
  for (const k of ["cargo", "deploy"]) {
    const t = read(root, k);
    if (!CARGO_RE.test(t)) throw new Error(`${F[k]}: no [workspace.package] version`);
    writeFileSync(join(root, F[k]), t.replace(CARGO_RE, `$1${version}$3`));
  }
  // shortcut: first `"version": "…"` is the top-level one in both JSON files; replace in text to keep formatting.
  for (const k of ["npm", "tauri"]) {
    const t = read(root, k);
    if (!/"version"\s*:\s*"[^"]*"/.test(t)) throw new Error(`${F[k]}: no version`);
    writeFileSync(join(root, F[k]), t.replace(/("version"\s*:\s*")[^"]*(")/, `$1${version}$2`));
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const root = join(dirname(fileURLToPath(import.meta.url)), "../..");
  const [cmd, arg] = process.argv.slice(2);
  try {
    if (cmd === "check") checkTag(arg ?? "", readVersions(root));
    else if (cmd === "bump") bump(root, arg ?? "");
    else throw new Error("usage: version.mjs check vX.Y.Z | bump X.Y.Z");
  } catch (e) {
    console.error(e.message);
    process.exit(1);
  }
}
