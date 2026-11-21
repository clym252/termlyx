// Tauri v2 updater manifest (latest.json) from the release files.
// Fails unless every platform has exactly one bundle and a non-empty .sig, so a
// partial manifest is never published.
import { readFileSync, readdirSync } from "node:fs";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

// The universal .app.tar.gz serves both Mac architectures.
const PLATFORMS = {
  "darwin-aarch64": ".app.tar.gz",
  "darwin-x86_64": ".app.tar.gz",
  "windows-x86_64": "-setup.exe",
};

export function buildManifest({ version, notes, pubDate, baseUrl, files }) {
  if (!/^\d+\.\d+\.\d+$/.test(version ?? "")) throw new Error(`version must be X.Y.Z, got "${version}"`);
  const base = baseUrl.replace(/\/+$/, "");
  const platforms = {};
  for (const [key, suffix] of Object.entries(PLATFORMS)) {
    const found = files.filter((f) => f.endsWith(suffix) && files.includes(`${f}.sig`));
    if (found.length !== 1) {
      throw new Error(`${key}: need exactly one *${suffix} with a .sig, found ${found.length}`);
    }
    const [file] = found;
    const name = basename(file);
    // GitHub renames assets with other characters, which would break the URL.
    if (!/^[\w.-]+$/.test(name)) throw new Error(`${key}: unsafe asset name "${name}"`);
    const signature = readFileSync(`${file}.sig`, "utf8").trim();
    if (!signature) throw new Error(`${key}: ${name}.sig is empty`);
    platforms[key] = { signature, url: `${base}/${name}` };
  }
  return { version, notes, pub_date: new Date(pubDate).toISOString(), platforms };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    const { values: o } = parseArgs({
      options: {
        version: { type: "string" },
        notes: { type: "string" },
        dir: { type: "string" },
        "base-url": { type: "string" },
      },
    });
    for (const k of ["version", "notes", "dir", "base-url"]) if (!o[k]) throw new Error(`missing --${k}`);
    const manifest = buildManifest({
      version: o.version,
      notes: readFileSync(o.notes, "utf8").trim(),
      pubDate: new Date(),
      baseUrl: o["base-url"],
      files: readdirSync(o.dir).map((f) => join(o.dir, f)),
    });
    process.stdout.write(JSON.stringify(manifest, null, 2) + "\n");
  } catch (e) {
    console.error(`latest-json: ${e.message}`);
    process.exit(1);
  }
}
