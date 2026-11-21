import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const css = readFileSync(`${__dirname}/../ui.css`, "utf8");

// Everything after the first `:root { … }` block, where the dark defaults live.
const rules = css.slice(css.indexOf("}", css.indexOf(":root")) + 1);

describe("ui.css", () => {
  it("takes every colour from a theme variable", () => {
    // Shadows and scrims stay black at an opacity: they darken whatever is under them in any theme.
    const literals = rules.match(/#[0-9a-fA-F]{3,8}\b|rgba?\((?!0[ ,]+0[ ,]+0\s*[/,])[^)]*\)/g) ?? [];
    expect(literals).toEqual([]);
  });

  it("has no text below 12px", () => {
    expect(rules.match(/font-size:\s*(?:[0-9]|1[01])px/g) ?? []).toEqual([]);
  });

  it("never fades an inactive pane's terminal text", () => {
    expect(rules).not.toMatch(/\.terminal-host\s*\{[^}]*opacity/);
  });
});
