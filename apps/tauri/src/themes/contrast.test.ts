import { describe, expect, it } from "vitest";
import { THEMES, findTheme } from ".";
import { contrastRatio, nudgeToContrast, themeContrastFailures } from "./contrast";

describe("contrast", () => {
  it("contrastRatio matches WCAG reference values", () => {
    expect(contrastRatio("#000000", "#ffffff")).toBeCloseTo(21, 1);
    expect(contrastRatio("#777777", "#ffffff")).toBeCloseTo(4.48, 2);
    expect(contrastRatio("#ffffff", "#777777")).toBeCloseTo(4.48, 2);
  });

  it("nudgeToContrast keeps a passing colour and fixes a failing one", () => {
    expect(nudgeToContrast("#000000", "#ffffff", 4.5, true)).toBe("#000000");
    const darker = nudgeToContrast("#cccc00", "#ffffff", 3, true);
    expect(contrastRatio(darker, "#ffffff")).toBeGreaterThanOrEqual(3);
    const lighter = nudgeToContrast("#333366", "#000000", 4.5, false);
    expect(contrastRatio(lighter, "#000000")).toBeGreaterThanOrEqual(4.5);
  });

  it("themeContrastFailures names the failing pair", () => {
    const t = THEMES[0];
    const bad = { ...t, terminal: { ...t.terminal, foreground: t.terminal.background } };
    expect(themeContrastFailures(bad).some((f) => f.includes("foreground"))).toBe(true);
  });

  it.each(THEMES.map((t) => [t.id, t] as const))("%s meets every floor", (_id, t) => {
    expect(themeContrastFailures(t)).toEqual([]);
  });

  it("ships no Solarized theme; a host naming it gets the app theme", () => {
    expect(THEMES.some((t) => t.id.startsWith("solarized"))).toBe(false);
    expect(findTheme("Solarized Light")).toBeUndefined();
  });
});
