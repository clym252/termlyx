import { describe, expect, it } from "vitest";
import { isInTerminal, isPaletteKey, paletteLabel } from "./hotkeys";

type Mods = Partial<{ meta: boolean; ctrl: boolean; alt: boolean; shift: boolean }>;

function k(mods: Mods, key = "k") {
  return {
    key,
    code: `Key${key.toUpperCase()}`,
    metaKey: !!mods.meta,
    ctrlKey: !!mods.ctrl,
    altKey: !!mods.alt,
    shiftKey: !!mods.shift,
  };
}

describe("isPaletteKey on macOS", () => {
  it("is Cmd+K, in or out of a terminal", () => {
    expect(isPaletteKey(k({ meta: true }), false, "mac")).toBe(true);
    expect(isPaletteKey(k({ meta: true }), true, "mac")).toBe(true);
  });

  it("is Cmd+K on iOS too (hardware keyboard), never Ctrl+K", () => {
    expect(isPaletteKey(k({ meta: true }), true, "ios")).toBe(true);
    expect(isPaletteKey(k({ ctrl: true }), false, "ios")).toBe(false);
  });

  it("leaves Ctrl+K to the shell", () => {
    expect(isPaletteKey(k({ ctrl: true }), false, "mac")).toBe(false);
    expect(isPaletteKey(k({ ctrl: true }), true, "mac")).toBe(false);
    expect(isPaletteKey(k({ ctrl: true, shift: true }, "K"), false, "mac")).toBe(false);
  });

  it("ignores other modifiers and keys", () => {
    expect(isPaletteKey(k({ meta: true, shift: true }, "K"), false, "mac")).toBe(false);
    expect(isPaletteKey(k({ meta: true, alt: true }), false, "mac")).toBe(false);
    expect(isPaletteKey(k({ meta: true }, "j"), false, "mac")).toBe(false);
  });
});

describe.each(["windows", "other"] as const)("isPaletteKey on %s", (platform) => {
  it("is Ctrl+Shift+K everywhere", () => {
    expect(isPaletteKey(k({ ctrl: true, shift: true }, "K"), false, platform)).toBe(true);
    expect(isPaletteKey(k({ ctrl: true, shift: true }, "K"), true, platform)).toBe(true);
  });

  it("is plain Ctrl+K only outside a terminal", () => {
    expect(isPaletteKey(k({ ctrl: true }), false, platform)).toBe(true);
    expect(isPaletteKey(k({ ctrl: true }), true, platform)).toBe(false);
  });

  it("ignores Cmd, Alt and other keys", () => {
    expect(isPaletteKey(k({ meta: true }), false, platform)).toBe(false);
    expect(isPaletteKey(k({ ctrl: true, alt: true }), false, platform)).toBe(false);
    expect(isPaletteKey(k({ ctrl: true, shift: true, alt: true }, "K"), false, platform)).toBe(false);
    expect(isPaletteKey(k({ ctrl: true }, "j"), false, platform)).toBe(false);
    expect(isPaletteKey(k({}), false, platform)).toBe(false);
  });
});

describe("paletteLabel", () => {
  it("names the chord for the platform", () => {
    expect(paletteLabel("mac")).toBe("⌘K");
    expect(paletteLabel("ios")).toBe("⌘K");
    expect(paletteLabel("windows")).toBe("Ctrl+Shift+K");
    expect(paletteLabel("other")).toBe("Ctrl+Shift+K");
  });
});

describe("isInTerminal", () => {
  it("is true for a target inside .xterm only", () => {
    document.body.innerHTML = `<div class="xterm"><textarea id="in"></textarea></div><input id="out" />`;
    expect(isInTerminal(document.getElementById("in"))).toBe(true);
    expect(isInTerminal(document.getElementById("out"))).toBe(false);
    expect(isInTerminal(null)).toBe(false);
    expect(isInTerminal(window)).toBe(false);
  });
});
