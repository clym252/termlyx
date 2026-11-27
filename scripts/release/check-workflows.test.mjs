import { test } from "node:test";
import assert from "node:assert/strict";
import { checkToolchain, checkWorkflow } from "./check-workflows.mjs";

const SHA = "11bd71901bbe5b1630ceea73d27597364c9af683";

const good = `name: ok
on:
  push:
    tags: ["v*"]
permissions:
  contents: read
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@${SHA} # v4.2.2
      - uses: ./.github/actions/local
      - run: echo \${{ secrets.GITHUB_TOKEN }}
  scan:
    runs-on: ubuntu-latest
    environment: personal-scan
    steps:
      - run: echo \${{ secrets.PERSONAL_SCAN_PATTERNS }}
  sign:
    runs-on: macos-latest
    environment: release
    permissions:
      contents: write
    steps:
      - run: echo hi
        env:
          KEY: \${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
  sign2:
    runs-on: macos-latest
    environment:
      name: "release"
    steps:
      - run: echo \${{ secrets.APPLE_API_KEY }}
`;

const one = (yaml, re) => {
  const p = checkWorkflow(yaml);
  assert.ok(p.some((x) => re.test(x)), `expected ${re} in ${JSON.stringify(p)}`);
};

test("passes a SHA-pinned, read-only sample", () => {
  assert.deepEqual(checkWorkflow(good), []);
});

test("flags an action pinned to a tag", () => {
  one(good.replace(`actions/checkout@${SHA} # v4.2.2`, "actions/checkout@v4"), /line 11: .*actions\/checkout@v4/);
});

test("flags a SHA pin without a version comment", () => {
  one(good.replace(`@${SHA} # v4.2.2`, `@${SHA}`), /version comment/);
});

test("flags pull_request_target", () => {
  one(good.replace("  push:\n", "  pull_request_target:\n  push:\n"), /pull_request_target/);
  one(good.replace("on:\n  push:\n    tags: [\"v*\"]", "on: [push, pull_request_target]"), /pull_request_target/);
});

test("flags a workflow without top-level permissions", () => {
  one(good.replace("permissions:\n  contents: read\n", ""), /top-level permissions/);
});

test("flags write access in top-level permissions", () => {
  one(good.replace("  contents: read\n", "  contents: write\n"), /top-level permissions/);
  one(good.replace("permissions:\n  contents: read\n", "permissions: write-all\n"), /top-level permissions/);
});

test("flags a secret outside a release-environment job", () => {
  one(good.replace("secrets.GITHUB_TOKEN", "secrets.APPLE_CERTIFICATE"), /line 13: .*APPLE_CERTIFICATE/);
  one(good.replace("secrets.GITHUB_TOKEN", "secrets['APPLE_CERTIFICATE']"), /secrets/);
  one(good.replace("secrets.GITHUB_TOKEN", "toJSON(secrets)"), /secrets/);
  one(good.replace("environment: release", "environment: staging"), /TAURI_SIGNING_PRIVATE_KEY/);
  one(good.replace("secrets.PERSONAL_SCAN_PATTERNS", "secrets.APPLE_CERTIFICATE"), /APPLE_CERTIFICATE used outside a job with environment: release/);
});

test("allows the scan patterns only in the personal-scan environment", () => {
  one(good.replace("secrets.GITHUB_TOKEN", "secrets.PERSONAL_SCAN_PATTERNS"), /line 13: .*PERSONAL_SCAN_PATTERNS.*personal-scan/);
  one(good.replace("environment: personal-scan", "environment: staging"), /PERSONAL_SCAN_PATTERNS.*personal-scan/);
  const flow = good.replace("environment: personal-scan", "environment: { name: personal-scan }");
  assert.deepEqual(checkWorkflow(flow), []);
  // release may use any secret, the scan patterns included.
  assert.deepEqual(checkWorkflow(good.replace("secrets.TAURI_SIGNING_PRIVATE_KEY", "secrets.PERSONAL_SCAN_PATTERNS")), []);
});

test("flags a secret in top-level env", () => {
  one(good.replace("jobs:\n", "env:\n  X: ${{ secrets.NPM_TOKEN }}\njobs:\n"), /NPM_TOKEN/);
});

test("flags secrets: inherit outside the release environment", () => {
  one(good.replace("      - run: echo hi\n", "      - run: echo hi\n").replace("  build:\n    runs-on: ubuntu-latest\n", "  build:\n    uses: ./.github/workflows/x.yml\n    secrets: inherit\n    runs-on: ubuntu-latest\n"), /inherit/);
});

const DL = "d3f86a106a0bac45b974a628896c90dbdf5c8093";
const withDownload = (w) =>
  good.replace("      - uses: ./.github/actions/local\n", `      - uses: actions/download-artifact@${DL} # v4.3.0\n${w}      - uses: ./.github/actions/local\n`);

test("flags download-artifact without a name or pattern", () => {
  one(withDownload("        with:\n          path: dist\n          merge-multiple: true\n"), /line 12: .*download-artifact.*name or pattern/);
  one(withDownload(""), /download-artifact.*name or pattern/);
});

test("passes download-artifact filtered by name or pattern", () => {
  assert.deepEqual(checkWorkflow(withDownload("        with:\n          pattern: release-*\n          path: dist\n")), []);
  assert.deepEqual(checkWorkflow(withDownload("        with:\n          name: mac\n")), []);
});

test("checkToolchain: Dockerfile rust tag must match the toolchain file and be digest-pinned", () => {
  const d = "a".repeat(64);
  const docker = (v) => `FROM rust:${v}-trixie@sha256:${d} AS build\nFROM debian:trixie-slim@sha256:${d}\n`;
  const toml = (v) => `[toolchain]\nchannel = "${v}"\n`;
  assert.deepEqual(checkToolchain(docker("1.99.1"), toml("1.99.1")), []);
  assert.equal(checkToolchain(docker("1.99.2"), toml("1.99.1")).length, 1);
  assert.equal(checkToolchain(docker("1.99.1"), toml("stable")).length >= 1, true);
  assert.equal(checkToolchain("FROM rust:1.99.1-trixie AS build\n", toml("1.99.1")).length, 1);
});
