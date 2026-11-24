import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { check } from "./check.mjs";

const VIDEO = `<video muted loop playsinline poster="/media/p.jpg" data-hero><source src="/media/a.mp4" type="video/mp4"><source src="/media/a.webm" type="video/webm"></video>`;
const page = (body) => `<!doctype html><html><body>${body}</body></html>`;

// Writes files into a fresh dist dir, runs check, returns the failed rule names.
function run(files) {
  const dist = mkdtempSync(join(tmpdir(), "check-"));
  for (const [name, content] of Object.entries(files)) {
    mkdirSync(dirname(join(dist, name)), { recursive: true });
    writeFileSync(join(dist, name), content);
  }
  return check(dist).map((f) => f.rule);
}

const clean = { "index.html": page(`<h1 id="top">Hi</h1><a href="#top">t</a><a href="/about">a</a><a href="https://example.com">e</a>`), "about.html": page("About") };

test("clean fixture passes", () => assert.deepEqual(run(clean), []));
test("no-external-assets: CDN script", () =>
  assert.deepEqual(run({ "index.html": page(`<script src="https://cdn.example.com/x.js"></script>`) }), ["no-external-assets"]));
test("no-external-assets: protocol-relative img", () =>
  assert.deepEqual(run({ "index.html": page(`<img src="//x.test/a.png">`) }), ["no-external-assets"]));
test("no-external-assets: absolute css url", () =>
  assert.deepEqual(run({ ...clean, "styles.css": `a{background:url(https://x.test/a.png)}` }), ["no-external-assets"]));
test("no-version: v0.1.1", () => assert.deepEqual(run({ "index.html": page("Get v0.1.1 now") }), ["no-version"]));
test("no-version: IP address is fine", () => assert.deepEqual(run({ "index.html": page("ssh 192.168.1.20") }), []));
test("no-competitor", () => assert.deepEqual(run({ "index.html": page(`Better than ${"Term" + "ius"}`) }), ["no-competitor"]));
test("anchors: dangling #nope", () => assert.deepEqual(run({ "index.html": page(`<a href="#nope">x</a>`) }), ["anchors"]));
test("anchors: missing local page", () => assert.deepEqual(run({ "index.html": page(`<a href="/gone">x</a>`) }), ["anchors"]));
test("anchors: /x resolves to x/index.html", () =>
  assert.deepEqual(run({ "index.html": page(`<a href="/docs">x</a>`), "docs/index.html": page("d") }), []));
test("media-budget: 4 MiB mp4", () =>
  assert.deepEqual(run({ ...clean, "media/big.mp4": Buffer.alloc(4 * 1024 * 1024) }), ["media-budget"]));
test("media-budget: 300 KiB jpg", () =>
  assert.deepEqual(run({ ...clean, "media/big.jpg": Buffer.alloc(300 * 1024) }), ["media-budget"]));
test("video-attrs: missing muted", () =>
  assert.deepEqual(run({ "index.html": page(VIDEO.replace(" muted", "")), "media/p.jpg": "x" }), ["video-attrs"]));
test("video-attrs: missing poster file", () =>
  assert.deepEqual(run({ "index.html": page(VIDEO) }), ["video-attrs"]));
test("video-attrs: non-hero needs preload=none", () =>
  assert.deepEqual(run({ "index.html": page(VIDEO.replace(" data-hero", "")), "media/p.jpg": "x" }), ["video-attrs"]));
test("video-attrs: valid hero and lazy video pass", () =>
  assert.deepEqual(run({ "index.html": page(VIDEO + VIDEO.replace(" data-hero", ' preload="none"')), "media/p.jpg": "x" }), []));
