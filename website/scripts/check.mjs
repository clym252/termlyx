// Static checks over the built site. Regex scanning is enough for our hand-written pages.
import { readdirSync, readFileSync, statSync, existsSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

// Built from parts so this source never contains the name it forbids.
const COMPETITOR = new RegExp("term" + "ius", "i");
const MiB = 1024 * 1024;
const walk = (d) =>
  readdirSync(d, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(join(d, e.name)) : [join(d, e.name)]));
const attr = (tag, name) => tag.match(new RegExp(`\\s${name}\\s*=\\s*(?:"([^"]*)"|'([^']*)')`, "i"))?.slice(1).find((v) => v !== undefined);
const hasAttr = (tag, name) => new RegExp(`\\s${name}(?=[\\s=>/])`, "i").test(tag);

export function check(dist) {
  const failures = [];
  const files = walk(dist);
  const fail = (file, rule, detail) => failures.push({ file: relative(dist, file), rule, detail });
  const resolves = (p) => {
    const base = join(dist, p.split(/[?#]/)[0]);
    return [base, `${base}.html`, join(base, "index.html")].some((c) => existsSync(c) && statSync(c).isFile());
  };

  for (const f of files) {
    const size = statSync(f).size;
    if (/\.(mp4|webm)$/.test(f) && size > 3 * MiB) fail(f, "media-budget", `${size} bytes > 3 MiB`);
    if (f.endsWith(".jpg") && size > 200 * 1024) fail(f, "media-budget", `${size} bytes > 200 KiB`);
    const buf = readFileSync(f);
    if (buf.includes("\0")) { // binary: only the competitor scan applies
      if (COMPETITOR.test(buf.toString("latin1"))) fail(f, "no-competitor", "found");
      continue;
    }
    const src = buf.toString("utf8");
    if (COMPETITOR.test(src)) fail(f, "no-competitor", "found");

    if (f.endsWith(".css")) {
      for (const m of src.matchAll(/url\(\s*["']?\s*((?:https?:)?\/\/[^)"']*)/gi)) fail(f, "no-external-assets", m[1]);
    }
    if (!f.endsWith(".html")) continue;

    for (const [tag] of src.matchAll(/<(script|link|img|video|source|iframe)\b[^>]*>/gi)) {
      for (const n of ["src", "href", "poster"]) {
        const v = attr(tag, n);
        if (v && /^(https?:)?\/\//i.test(v.trim())) fail(f, "no-external-assets", `${n}=${v}`);
      }
    }

    // Strip until stable: one pass can leave a tag behind (e.g. "<scr<script>ipt>").
    let text = src, prev;
    do {
      prev = text;
      text = text.replace(/<(script|style)\b[\s\S]*?<\/\1>/gi, "").replace(/<[^>]*>/g, " ");
    } while (text !== prev);
    for (const m of text.matchAll(/\bv\d+\.\d+\.\d+\b/g)) fail(f, "no-version", m[0]);

    const ids = new Set([...src.matchAll(/\sid\s*=\s*["']([^"']*)["']/gi)].map((m) => m[1]));
    for (const [tag] of src.matchAll(/<a\b[^>]*>/gi)) {
      const h = attr(tag, "href");
      if (h?.startsWith("#") && h.length > 1 && !ids.has(h.slice(1))) fail(f, "anchors", `no id for ${h}`);
      else if (h?.startsWith("/") && !h.startsWith("//") && !resolves(h)) fail(f, "anchors", `${h} not in dist`);
    }

    for (const [vid] of src.matchAll(/<video\b[\s\S]*?<\/video>/gi)) {
      const open = vid.match(/<video\b[^>]*>/i)[0];
      const miss = ["muted", "loop", "playsinline"].filter((a) => !hasAttr(open, a));
      const poster = attr(open, "poster");
      if (!poster || !resolves(poster)) miss.push(`poster (${poster ?? "none"})`);
      const exts = [...vid.matchAll(/<source\b[^>]*>/gi)].map((s) => attr(s[0], "src") ?? "");
      for (const e of [".mp4", ".webm"]) if (!exts.some((s) => s.endsWith(e))) miss.push(`${e} source`);
      const hero = hasAttr(open, "data-hero");
      if (!hero && attr(open, "preload") !== "none") miss.push('preload="none"');
      if (hero && attr(open, "preload") === "none") miss.push("hero must not be preload=none");
      if (miss.length) fail(f, "video-attrs", miss.join(", "));
    }
  }
  return failures;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const dist = fileURLToPath(new URL("../dist", import.meta.url));
  if (!existsSync(dist)) { console.error("dist/ missing: run pnpm build first"); process.exit(1); }
  const failures = check(dist);
  for (const f of failures) console.error(`${f.file}: ${f.rule}: ${f.detail}`);
  console.log(failures.length ? `${failures.length} check failure(s)` : "checks passed");
  process.exit(failures.length ? 1 : 0);
}
