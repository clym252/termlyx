import { cpSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const at = (p) => root + p;
rmSync(at("dist"), { recursive: true, force: true });
cpSync(at("src"), at("dist"), { recursive: true, filter: (s) => !s.endsWith("/src/styles.css") });
execFileSync(at("node_modules/.bin/tailwindcss"), ["-i", at("src/styles.css"), "-o", at("dist/styles.css"), "--minify"], { stdio: "inherit" });
