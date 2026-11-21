import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";
import { findTheme } from "../themes";

const SRC = join(__dirname, "..");
const SKIP = [/\.test\./, /^api\/mock\.ts$/, /^themes\/palettes\//, /^themes\/index\.ts$/];

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((n) => {
    const p = join(dir, n);
    return statSync(p).isDirectory() ? files(p) : /\.tsx?$/.test(n) ? [p] : [];
  });
}

describe("brand", () => {
  it('has no "Tether" in UI source', () => {
    const bad = files(SRC)
      .map((p) => relative(SRC, p))
      .filter((p) => !SKIP.some((re) => re.test(p)))
      .filter((p) => {
        // Comments are not user-visible; \b lets identifiers like TetherApi pass.
        const code = readFileSync(join(SRC, p), "utf8").replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|\s)\/\/.*$/gm, "");
        return /\bTether\b/.test(code);
      });
    expect(bad).toEqual([]);
  });

  it("old theme names still resolve", () => {
    expect(findTheme("Tether Light")?.id).toBe("tether-light");
    expect(findTheme("Tether Dark")?.id).toBe("tether-dark");
    expect(findTheme("Tether High Contrast Dark")?.id).toBe("tether-high-contrast-dark");
    expect(findTheme("Termlyx Light")?.id).toBe("tether-light");
  });
});
