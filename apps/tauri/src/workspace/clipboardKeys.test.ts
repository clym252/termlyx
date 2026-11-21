import { describe, expect, it, vi } from "vitest";
import { clipboardAction, terminalKeyHandler } from "./clipboardKeys";

type Mods = Partial<{ meta: boolean; ctrl: boolean; alt: boolean; shift: boolean }>;

function k(letter: string, mods: Mods, type = "keydown") {
  return {
    type,
    key: mods.shift ? letter.toUpperCase() : letter,
    code: `Key${letter.toUpperCase()}`,
    metaKey: !!mods.meta,
    ctrlKey: !!mods.ctrl,
    altKey: !!mods.alt,
    shiftKey: !!mods.shift,
  };
}

describe("clipboardAction on macOS", () => {
  it("is always null: the native Cmd+C and Cmd+V are unchanged", () => {
    for (const sel of [true, false]) {
      expect(clipboardAction(k("c", { meta: true }), sel, "mac")).toBeNull();
      expect(clipboardAction(k("v", { meta: true }), sel, "mac")).toBeNull();
      expect(clipboardAction(k("c", { ctrl: true }), sel, "mac")).toBeNull();
      expect(clipboardAction(k("v", { ctrl: true }), sel, "mac")).toBeNull();
      expect(clipboardAction(k("c", { ctrl: true }), sel, "ios")).toBeNull();
    }
  });
});

describe.each(["windows", "other"] as const)("clipboardAction on %s", (platform) => {
  const a = (e: ReturnType<typeof k>, sel: boolean) => clipboardAction(e, sel, platform);

  it("Ctrl+C copies a selection", () => {
    expect(a(k("c", { ctrl: true }), true)).toBe("copy");
  });

  it("Ctrl+C with no selection reaches the shell as ^C", () => {
    expect(a(k("c", { ctrl: true }), false)).toBeNull();
  });

  it("Ctrl+Shift+C copies a selection and does nothing without one", () => {
    expect(a(k("c", { ctrl: true, shift: true }), true)).toBe("copy");
    expect(a(k("c", { ctrl: true, shift: true }), false)).toBeNull();
  });

  it("Ctrl+V and Ctrl+Shift+V paste, selection or not", () => {
    for (const sel of [true, false]) {
      expect(a(k("v", { ctrl: true }), sel)).toBe("paste");
      expect(a(k("v", { ctrl: true, shift: true }), sel)).toBe("paste");
    }
  });

  it("ignores keyup and keypress", () => {
    expect(a(k("c", { ctrl: true }, "keyup"), true)).toBeNull();
    expect(a(k("v", { ctrl: true }, "keyup"), true)).toBeNull();
    expect(a(k("v", { ctrl: true }, "keypress"), true)).toBeNull();
  });

  it("ignores Alt, Cmd and other keys", () => {
    expect(a(k("c", { ctrl: true, alt: true }), true)).toBeNull();
    expect(a(k("v", { ctrl: true, alt: true }), true)).toBeNull();
    expect(a(k("c", { meta: true }), true)).toBeNull();
    expect(a(k("c", { ctrl: true, meta: true }), true)).toBeNull();
    expect(a(k("c", {}), true)).toBeNull();
    expect(a(k("x", { ctrl: true }), true)).toBeNull();
  });
});

function fakeTerm(selection: string) {
  let sel = selection;
  return {
    hasSelection: () => sel !== "",
    getSelection: () => sel,
    clearSelection: vi.fn(() => {
      sel = "";
    }),
  };
}

function event(letter: string, mods: Mods, type = "keydown") {
  return { ...k(letter, mods, type), preventDefault: vi.fn() };
}

describe("terminalKeyHandler on Windows", () => {
  function setup(selection: string) {
    const term = fakeTerm(selection);
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    return { term, writeText, handler: terminalKeyHandler(term, "windows") };
  }

  it("Ctrl+C with a selection copies it, clears it and keeps ^C from the shell", () => {
    const { term, writeText, handler } = setup("hello");
    const e = event("c", { ctrl: true });
    expect(handler(e as unknown as KeyboardEvent)).toBe(false);
    expect(writeText).toHaveBeenCalledWith("hello");
    expect(term.clearSelection).toHaveBeenCalledTimes(1);
    expect(e.preventDefault).toHaveBeenCalled();
  });

  it("Ctrl+C with no selection reaches the shell", () => {
    const { term, writeText, handler } = setup("");
    const e = event("c", { ctrl: true });
    expect(handler(e as unknown as KeyboardEvent)).toBe(true);
    expect(writeText).not.toHaveBeenCalled();
    expect(term.clearSelection).not.toHaveBeenCalled();
    expect(e.preventDefault).not.toHaveBeenCalled();
  });

  it("Ctrl+V is left to the browser's paste event so xterm keeps bracketed paste", () => {
    const { handler } = setup("");
    const e = event("v", { ctrl: true });
    expect(handler(e as unknown as KeyboardEvent)).toBe(false);
    expect(e.preventDefault).not.toHaveBeenCalled();
  });

  it("the palette chord and workspace shortcuts are not terminal input", () => {
    const { handler } = setup("");
    expect(handler(event("k", { ctrl: true, shift: true }) as unknown as KeyboardEvent)).toBe(false);
    expect(handler(event("t", { ctrl: true, shift: true }) as unknown as KeyboardEvent)).toBe(false);
  });

  it("the sidebar toggle is not terminal input, but plain Ctrl+B is", () => {
    const { handler } = setup("");
    expect(handler(event("b", { ctrl: true, shift: true }) as unknown as KeyboardEvent)).toBe(false);
    expect(handler(event("b", { ctrl: true }) as unknown as KeyboardEvent)).toBe(true);
  });

  it("plain Ctrl+K, Ctrl+T, Ctrl+W and Ctrl+D reach the shell", () => {
    const { handler } = setup("");
    for (const l of ["k", "t", "w", "d"]) {
      expect(handler(event(l, { ctrl: true }) as unknown as KeyboardEvent)).toBe(true);
    }
  });
});

describe("terminalKeyHandler on macOS", () => {
  it("leaves Cmd+C and Ctrl+C to xterm and the native clipboard", () => {
    const handler = terminalKeyHandler(fakeTerm("hello"), "mac");
    expect(handler(event("c", { meta: true }) as unknown as KeyboardEvent)).toBe(true);
    expect(handler(event("c", { ctrl: true }) as unknown as KeyboardEvent)).toBe(true);
    expect(handler(event("v", { meta: true }) as unknown as KeyboardEvent)).toBe(true);
  });

  it("Cmd+K opens the palette, so the terminal drops it; Ctrl+K reaches the shell", () => {
    const handler = terminalKeyHandler(fakeTerm(""), "mac");
    expect(handler(event("k", { meta: true }) as unknown as KeyboardEvent)).toBe(false);
    expect(handler(event("k", { ctrl: true }) as unknown as KeyboardEvent)).toBe(true);
  });
});
