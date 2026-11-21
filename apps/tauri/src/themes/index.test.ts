import { describe, expect, it } from "vitest";
import { THEMES, isThemeId, themeGroups } from ".";

describe("themes", () => {
  it("ships exactly the spec's 30 themes", () => {
    expect(THEMES).toHaveLength(30);
    expect(new Set(THEMES.map((t) => t.id)).size).toBe(30);
    const { dark, light } = themeGroups();
    expect(dark).toHaveLength(20);
    expect(light).toHaveLength(10);
    expect(dark[0].id).toBe("tether-dark");
    expect(light[0].id).toBe("tether-light");
    // Tether's own first, then by name.
    const rest = dark.filter((t) => !t.id.startsWith("tether")).map((t) => t.name);
    expect(rest).toEqual([...rest].sort((a, b) => a.localeCompare(b)));
  });

  it("rejected themes are absent", () => {
    for (const id of ["catppuccin-frappe", "kanagawa-lotus", "rose-pine-dawn", "one-dark", "solarized-dark"]) {
      expect(isThemeId(id)).toBe(false);
    }
  });
});
