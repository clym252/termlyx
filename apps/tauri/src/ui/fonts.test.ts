import { describe, expect, it } from "vitest";
import { canvasCheck, familyOf, installedFonts, stackFor } from "./fonts";

describe("fonts", () => {
  it("keeps the bundled font and the installed candidates, in order", () => {
    expect(installedFonts((f) => f === "Menlo")).toEqual(["JetBrains Mono", "Menlo"]);
  });
  it("takes the first family of a stack", () => {
    expect(familyOf('"SF Mono", Menlo, monospace')).toBe("SF Mono");
    expect(familyOf("Menlo, Monaco")).toBe("Menlo");
  });
  it("builds a stack", () => expect(stackFor("Menlo")).toBe('"Menlo", monospace'));
  it("does not throw without canvas", () => expect(canvasCheck("Menlo")).toBe(false));
});
