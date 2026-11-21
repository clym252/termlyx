import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { checkTag, bump, readVersions } from "./version.mjs";

const v = { cargo: "0.1.0", deploy: "0.1.0", npm: "0.1.0", tauri: "0.1.0" };

test("checkTag accepts a matching tag", () => {
  checkTag("v0.1.0", v);
});

test("checkTag names all four files on mismatch", () => {
  assert.throws(
    () => checkTag("v0.1.1", v),
    (e) =>
      /^tag v0\.1\.1 does not match: Cargo\.toml/.test(e.message) &&
      /server\/deploy\/Cargo\.toml/.test(e.message) &&
      /package\.json/.test(e.message) &&
      /tauri\.conf\.json/.test(e.message),
  );
});

test("checkTag flags the server deploy manifest alone", () => {
  assert.throws(() => checkTag("v0.1.0", { ...v, deploy: "0.0.9" }), /server\/deploy\/Cargo\.toml \(0\.0\.9\)/);
});

test("checkTag rejects malformed tags", () => {
  for (const t of ["v0.1", "v0.1.0-beta"]) assert.throws(() => checkTag(t, v), /tag must be vX\.Y\.Z/);
});

test("bump sets all four and leaves dependency versions", () => {
  const root = mkdtempSync(join(tmpdir(), "ver-"));
  mkdirSync(join(root, "apps/tauri/src-tauri"), { recursive: true });
  mkdirSync(join(root, "server/deploy"), { recursive: true });
  const manifest =
    '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.1.0"\nedition = "2024"\n\n[workspace.dependencies]\nserde = { version = "1", features = [] }\nfoo = "0.1.0"\n\n[other]\nversion = "0.1.0"\n';
  writeFileSync(join(root, "Cargo.toml"), manifest);
  writeFileSync(join(root, "server/deploy/Cargo.toml"), "# server only\n" + manifest);
  writeFileSync(join(root, "apps/tauri/package.json"), '{\n  "name": "x",\n  "version": "0.1.0"\n}\n');
  writeFileSync(join(root, "apps/tauri/src-tauri/tauri.conf.json"), '{\n  "version": "0.1.0"\n}\n');
  bump(root, "0.2.0");
  assert.deepEqual(readVersions(root), { cargo: "0.2.0", deploy: "0.2.0", npm: "0.2.0", tauri: "0.2.0" });
  for (const f of ["Cargo.toml", "server/deploy/Cargo.toml"]) {
    const c = readFileSync(join(root, f), "utf8");
    assert.match(c, /foo = "0\.1\.0"/);
    assert.match(c, /\[other\]\nversion = "0\.1\.0"/);
  }
});
