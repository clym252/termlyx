// Workflow policy: actions pinned to a full commit SHA with a version comment,
// no pull_request_target, read-only top-level permissions, and secrets only in
// jobs that run in the protected `release` environment.
// Line-based on purpose (no YAML library); it fails closed on anything it
// cannot place, such as `secrets[...]` or `toJSON(secrets)`.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const ALLOWED_SECRETS = new Set(["GITHUB_TOKEN", "PERSONAL_SCAN_PATTERNS"]);
const indent = (l) => l.length - l.trimStart().length;
const blank = (l) => /^\s*(#|$)/.test(l);
const unquote = (s) => s.trim().replace(/^(["'])(.*)\1$/, "$2");

// For each line, the job it belongs to (or null) and whether that job runs in `release`.
function jobsOf(lines) {
  const job = lines.map(() => null);
  const release = new Set();
  const start = lines.findIndex((l) => /^jobs:\s*(#.*)?$/.test(l));
  if (start < 0) return { job, release };
  let jobIndent = -1;
  let current = null;
  let envLine = -1; // index of a block-style `environment:` in the current job
  for (let i = start + 1; i < lines.length; i++) {
    const l = lines[i];
    if (blank(l)) {
      job[i] = current;
      continue;
    }
    const n = indent(l);
    if (n === 0) break; // next top-level key
    if (jobIndent < 0) jobIndent = n;
    if (n === jobIndent) {
      current = l.trim().replace(/:.*$/, "");
      envLine = -1;
    } else if (n === jobIndent + 2) {
      envLine = -1;
      const m = /^environment:\s*(.*?)\s*(#.*)?$/.exec(l.trim());
      if (m && m[1]) {
        if (unquote(m[1]) === "release" || /^\{\s*name:\s*["']?release["']?\s*[,}]/.test(m[1])) release.add(current);
      } else if (m) envLine = i;
    } else if (envLine >= 0 && n === jobIndent + 4) {
      const m = /^name:\s*(.*?)\s*(#.*)?$/.exec(l.trim());
      if (m && unquote(m[1]) === "release") release.add(current);
    }
    job[i] = current;
  }
  return { job, release };
}

// Whether the step whose `uses:` is on line i has `with: name:` or `with: pattern:` after it.
function downloadFiltered(lines, i) {
  const key = indent(lines[i]) + (/^\s*-\s/.test(lines[i]) ? 2 : 0);
  let inWith = false;
  for (let j = i + 1; j < lines.length; j++) {
    const l = lines[j];
    if (blank(l)) continue;
    const n = indent(l);
    if (n < key) break;
    if (n === key) inWith = /^with:\s*(#.*)?$/.test(l.trim());
    else if (inWith && n === key + 2 && /^(name|pattern):\s*\S/.test(l.trim())) return true;
  }
  return false;
}

export function checkWorkflow(text) {
  const lines = text.split(/\r?\n/);
  const problems = [];
  const at = (i, msg) => problems.push(`line ${i + 1}: ${msg}`);
  const { job, release } = jobsOf(lines);

  const permAt = lines.findIndex((l) => /^permissions:/.test(l));
  if (permAt < 0) problems.push("no top-level permissions");
  else {
    const block = [lines[permAt]];
    for (let i = permAt + 1; i < lines.length && (blank(lines[i]) || indent(lines[i]) > 0); i++) block.push(lines[i]);
    if (block.some((l) => /\bwrite/.test(l.replace(/#.*$/, "")))) at(permAt, "top-level permissions must be read-only");
  }

  lines.forEach((l, i) => {
    if (/\bpull_request_target\b/.test(l)) at(i, "pull_request_target is not allowed");

    const u = /^\s*(?:-\s+)?uses:\s*(\S+)\s*(.*)$/.exec(l);
    if (u) {
      const ref = unquote(u[1]);
      if (!ref.startsWith("./")) {
        if (!/^[\w.-]+\/[\w./-]+@[0-9a-f]{40}$/.test(ref)) at(i, `${ref} is not pinned to a full commit SHA`);
        else if (!/^#\s*v?\d/.test(u[2])) at(i, `${ref} needs a version comment (# vX.Y.Z)`);
        // An unfiltered download also pulls stray artifacts (e.g. docker build records) into a release.
        if (ref.startsWith("actions/download-artifact@") && !downloadFiltered(lines, i)) {
          at(i, "download-artifact needs a name or pattern under with:");
        }
      }
    }

    const inRelease = job[i] !== null && release.has(job[i]);
    if (/^\s*secrets:\s*inherit\b/.test(l) && !inRelease) at(i, "secrets: inherit outside the release environment");
    for (const expr of l.matchAll(/\$\{\{(.*?)\}\}/g)) {
      for (const s of expr[1].matchAll(/\bsecrets\b(\.([A-Za-z_][\w-]*))?/g)) {
        const name = s[2];
        if (name && ALLOWED_SECRETS.has(name)) continue;
        if (!inRelease) at(i, `secrets.${name ?? "?"} used outside a job with environment: release`);
      }
    }
  });
  return problems;
}

// The Dockerfile's `rust:<version>` tag must equal rust-toolchain.toml's channel (a
// Dependabot bump of one must not drift from the other) and both base images need a digest.
export function checkToolchain(dockerfile, toolchain) {
  const problems = [];
  const channel = /^channel\s*=\s*"([^"]+)"/m.exec(toolchain)?.[1];
  const from = [...dockerfile.matchAll(/^FROM\s+(\S+)/gm)].map((m) => m[1]);
  const rust = from.find((f) => f.startsWith("rust:"));
  if (!channel || !/^\d+\.\d+\.\d+$/.test(channel)) problems.push(`rust-toolchain.toml channel must be an exact version, got ${channel}`);
  if (!rust) problems.push("Dockerfile has no rust: build image");
  else if (rust.split(/[:@-]/)[1] !== channel) problems.push(`Dockerfile ${rust.split("@")[0]} does not match rust-toolchain.toml channel ${channel}`);
  for (const f of from) if (!/@sha256:[0-9a-f]{64}$/.test(f) && f !== "build") problems.push(`Dockerfile base image not pinned by digest: ${f}`);
  return problems;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const files = process.argv.slice(2);
  if (!files.length) {
    console.error("usage: check-workflows.mjs <workflow.yml>...");
    process.exit(2);
  }
  let bad = 0;
  for (const f of files) {
    for (const p of checkWorkflow(readFileSync(f, "utf8"))) {
      console.error(`${f}: ${p}`);
      bad++;
    }
  }
  for (const p of checkToolchain(readFileSync("server/Dockerfile", "utf8"), readFileSync("rust-toolchain.toml", "utf8"))) {
    console.error(`toolchain: ${p}`);
    bad++;
  }
  if (bad) process.exit(1);
  console.log(`check-workflows: ${files.length} file(s) ok`);
}
