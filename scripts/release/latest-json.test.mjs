import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildManifest } from "./latest-json.mjs";

const BASE = "https://github.com/clym252/termlyx/releases/download/v0.1.0";

function release(names) {
  const dir = mkdtempSync(join(tmpdir(), "latest-"));
  for (const [n, body] of Object.entries(names)) writeFileSync(join(dir, n), body);
  return Object.keys(names).map((n) => join(dir, n));
}

const full = () =>
  release({
    "Termlyx_0.1.0_universal.dmg": "dmg",
    "Termlyx.app.tar.gz": "tgz",
    "Termlyx.app.tar.gz.sig": "MAC-SIG\n",
    "Termlyx_0.1.0_x64-setup.exe": "exe",
    "Termlyx_0.1.0_x64-setup.exe.sig": "WIN-SIG\n",
    "termlyx-server-x86_64-unknown-linux-musl": "bin",
  });

const args = (files, over = {}) => ({
  version: "0.1.0",
  notes: "Beta.",
  pubDate: "2026-11-21T00:00:00.000Z",
  baseUrl: BASE + "/",
  files,
  ...over,
});

test("a full file set gives three platform keys with signatures from the .sig files", () => {
  const m = buildManifest(args(full()));
  assert.deepEqual(m, {
    version: "0.1.0",
    notes: "Beta.",
    pub_date: "2026-11-21T00:00:00.000Z",
    platforms: {
      "darwin-aarch64": { signature: "MAC-SIG", url: `${BASE}/Termlyx.app.tar.gz` },
      "darwin-x86_64": { signature: "MAC-SIG", url: `${BASE}/Termlyx.app.tar.gz` },
      "windows-x86_64": { signature: "WIN-SIG", url: `${BASE}/Termlyx_0.1.0_x64-setup.exe` },
    },
  });
});

test("buildManifest throws on missing platform", () => {
  const noWinSig = full().filter((f) => !f.endsWith("-setup.exe.sig"));
  assert.throws(() => buildManifest(args(noWinSig)), /windows-x86_64/);
  const noMac = full().filter((f) => !f.includes(".app.tar.gz"));
  assert.throws(() => buildManifest(args(noMac)), /darwin/);
});

test("buildManifest throws on an empty signature", () => {
  const files = release({ "Termlyx.app.tar.gz": "", "Termlyx.app.tar.gz.sig": "MAC", "a-setup.exe": "", "a-setup.exe.sig": " \n" });
  assert.throws(() => buildManifest(args(files)), /empty/);
});

test("buildManifest throws on two candidates for one platform", () => {
  const files = [...full(), ...release({ "Other_x64-setup.exe": "", "Other_x64-setup.exe.sig": "X" })];
  assert.throws(() => buildManifest(args(files)), /windows-x86_64/);
});

test("the version must be X.Y.Z", () => {
  for (const version of ["0.1", "v0.1.0", "0.1.0-beta", ""]) {
    assert.throws(() => buildManifest(args(full(), { version })), /X\.Y\.Z/);
  }
});
