import { describe, expect, it } from "vitest";
import { isSidebarToggleKey, matchShortcut, shortcutLabels, type WorkspaceCommand } from "./shortcuts";

type Mods = Partial<{ meta: boolean; ctrl: boolean; alt: boolean; shift: boolean }>;

function key(code: string, mods: Mods, keyName?: string) {
  return {
    code,
    key: keyName ?? (code.startsWith("Key") ? code.slice(3).toLowerCase() : code),
    metaKey: !!mods.meta,
    ctrlKey: !!mods.ctrl,
    altKey: !!mods.alt,
    shiftKey: !!mods.shift,
  };
}

const arrow = (dir: "Left" | "Right" | "Up" | "Down", mods: Mods) => key(`Arrow${dir}`, mods, `Arrow${dir}`);

const NEW_TAB: WorkspaceCommand = { kind: "newTab" };
const CLOSE: WorkspaceCommand = { kind: "close" };
const SPLIT_RIGHT: WorkspaceCommand = { kind: "split", orientation: "horizontal" };
const SPLIT_DOWN: WorkspaceCommand = { kind: "split", orientation: "vertical" };
const LOCAL: WorkspaceCommand = { kind: "localTerminal" };
const ZOOM: WorkspaceCommand = { kind: "zoom" };
const BROADCAST: WorkspaceCommand = { kind: "broadcast" };

describe("matchShortcut on macOS", () => {
  const m = (e: ReturnType<typeof key>) => matchShortcut(e, true);

  it("maps Cmd chords", () => {
    expect(m(key("KeyT", { meta: true }))).toEqual(NEW_TAB);
    expect(m(key("KeyW", { meta: true }))).toEqual(CLOSE);
    expect(m(key("KeyD", { meta: true }))).toEqual(SPLIT_RIGHT);
    expect(m(key("KeyD", { meta: true, shift: true }))).toEqual(SPLIT_DOWN);
    expect(m(key("KeyL", { meta: true, shift: true }))).toEqual(LOCAL);
    expect(m(arrow("Left", { meta: true, alt: true }))).toEqual({ kind: "focus", direction: "left" });
    expect(m(arrow("Down", { meta: true, alt: true }))).toEqual({ kind: "focus", direction: "down" });
  });

  it("mac_zoom_and_broadcast_chords", () => {
    expect(m(key("Enter", { meta: true, shift: true }, "Enter"))).toEqual(ZOOM);
    expect(m(key("KeyB", { meta: true, alt: true }, "b"))).toEqual(BROADCAST);
    // Layouts that make Alt+B a dead key still send the same `code`.
    expect(m(key("KeyB", { meta: true, alt: true }, "∫"))).toEqual(BROADCAST);
    expect(m(key("Enter", { meta: true }, "Enter"))).toBeNull();
    expect(m(key("Enter", { meta: true, shift: true, alt: true }, "Enter"))).toBeNull();
    expect(m(key("KeyB", { meta: true }, "b"))).toBeNull();
    expect(m(key("KeyB", { meta: true, alt: true, shift: true }, "B"))).toBeNull();
    // Cmd+Alt+I is the web inspector's, never ours.
    expect(m(key("KeyI", { meta: true, alt: true }, "i"))).toBeNull();
  });

  it("leaves Ctrl chords to the shell", () => {
    for (const code of ["KeyT", "KeyW", "KeyD", "KeyL", "KeyB", "Enter"]) {
      expect(m(key(code, { ctrl: true }))).toBeNull();
      expect(m(key(code, { ctrl: true, shift: true }))).toBeNull();
    }
    expect(m(key("KeyB", { ctrl: true, alt: true }))).toBeNull();
    expect(m(arrow("Left", { ctrl: true, alt: true }))).toBeNull();
  });

  it("ignores other combinations", () => {
    expect(m(key("KeyT", { meta: true, shift: true }))).toBeNull();
    expect(m(key("KeyW", { meta: true, shift: true }))).toBeNull();
    expect(m(key("KeyL", { meta: true }))).toBeNull();
    expect(m(key("KeyT", { meta: true, ctrl: true }))).toBeNull();
    expect(m(arrow("Left", { meta: true, alt: true, shift: true }))).toBeNull();
  });
});

describe("matchShortcut on Linux and Windows", () => {
  const m = (e: ReturnType<typeof key>) => matchShortcut(e, false);

  it("maps Ctrl+Shift chords", () => {
    expect(m(key("KeyT", { ctrl: true, shift: true }, "T"))).toEqual(NEW_TAB);
    expect(m(key("KeyW", { ctrl: true, shift: true }, "W"))).toEqual(CLOSE);
    expect(m(key("KeyD", { ctrl: true, shift: true }, "D"))).toEqual(SPLIT_RIGHT);
    expect(m(key("KeyD", { ctrl: true, alt: true, shift: true }, "D"))).toEqual(SPLIT_DOWN);
    expect(m(key("KeyL", { ctrl: true, shift: true }, "L"))).toEqual(LOCAL);
    expect(m(arrow("Right", { ctrl: true, alt: true }))).toEqual({ kind: "focus", direction: "right" });
    expect(m(arrow("Up", { ctrl: true, alt: true }))).toEqual({ kind: "focus", direction: "up" });
  });

  it("other_zoom_and_broadcast_chords", () => {
    expect(m(key("Enter", { ctrl: true, shift: true }, "Enter"))).toEqual(ZOOM);
    expect(m(key("KeyB", { ctrl: true, alt: true, shift: true }, "B"))).toEqual(BROADCAST);
    expect(m(key("Enter", { ctrl: true, alt: true, shift: true }, "Enter"))).toBeNull();
    // Ctrl+Shift+B is the sidebar toggle, not broadcast.
    expect(m(key("KeyB", { ctrl: true, shift: true }, "B"))).toBeNull();
    expect(isSidebarToggleKey(key("KeyB", { ctrl: true, shift: true }, "B"), false)).toBe(true);
    expect(m(key("KeyB", { ctrl: true, alt: true }, "b"))).toBeNull();
    expect(m(key("Enter", { meta: true, shift: true }, "Enter"))).toBeNull();
  });

  it("plain_ctrl_b_and_enter_stay_with_shell", () => {
    // Ctrl+B is tmux's prefix and readline's back-one-char, Ctrl+Enter a newline to a shell.
    expect(m(key("KeyB", { ctrl: true }, "b"))).toBeNull();
    expect(m(key("KeyB", { ctrl: true, alt: true }, "b"))).toBeNull();
    expect(m(key("Enter", { ctrl: true }, "Enter"))).toBeNull();
    expect(m(key("Enter", {}, "Enter"))).toBeNull();
    expect(matchShortcut(key("KeyB", { ctrl: true }, "b"), true)).toBeNull();
  });

  it("leaves plain Ctrl+T/W/D/L to the shell", () => {
    for (const code of ["KeyT", "KeyW", "KeyD", "KeyL"]) {
      expect(m(key(code, { ctrl: true }))).toBeNull();
    }
  });

  it("ignores Cmd/Super chords and other combinations", () => {
    expect(m(key("KeyT", { meta: true }))).toBeNull();
    expect(m(key("KeyW", { meta: true, shift: true }))).toBeNull();
    expect(m(key("KeyT", { ctrl: true, alt: true, shift: true }))).toBeNull();
    expect(m(key("KeyW", { ctrl: true, alt: true, shift: true }))).toBeNull();
    expect(m(arrow("Left", { ctrl: true, alt: true, shift: true }))).toBeNull();
    expect(m(arrow("Left", { ctrl: true }))).toBeNull();
  });
});

describe("matchShortcut on iOS (a hardware keyboard on an iPad)", () => {
  const m = (e: ReturnType<typeof key>) => matchShortcut(e, true, true);

  it("keeps the Mac chords for tabs", () => {
    expect(m(key("KeyT", { meta: true }))).toEqual(NEW_TAB);
    expect(m(key("KeyW", { meta: true }))).toEqual(CLOSE);
  });

  it("ios_allows_split_and_focus", () => {
    expect(m(key("KeyD", { meta: true }))).toEqual(SPLIT_RIGHT);
    expect(m(key("KeyD", { meta: true, shift: true }))).toEqual(SPLIT_DOWN);
    expect(m(arrow("Left", { meta: true, alt: true }))).toEqual({ kind: "focus", direction: "left" });
    expect(m(arrow("Down", { meta: true, alt: true }))).toEqual({ kind: "focus", direction: "down" });
    expect(m(key("Enter", { meta: true, shift: true }, "Enter"))).toEqual(ZOOM);
    expect(m(key("KeyB", { meta: true, alt: true }, "b"))).toEqual(BROADCAST);
  });

  it("ios_still_drops_local_terminal", () => {
    expect(m(key("KeyL", { meta: true, shift: true }))).toBeNull();
  });

  it("leaves Ctrl chords to the shell", () => {
    for (const code of ["KeyT", "KeyW", "KeyD", "KeyL"]) {
      expect(m(key(code, { ctrl: true }))).toBeNull();
    }
  });

  it("android_uses_ctrl_shift_and_drops_local_terminal", () => {
    const a = (e: ReturnType<typeof key>) => matchShortcut(e, false, true);
    expect(a(key("KeyD", { ctrl: true, shift: true }))).toEqual(SPLIT_RIGHT);
    expect(a(key("KeyL", { ctrl: true, shift: true }))).toBeNull();
  });

  it("is unchanged off iOS", () => {
    expect(matchShortcut(key("KeyD", { meta: true }), true, false)).toEqual(SPLIT_RIGHT);
    expect(matchShortcut(key("KeyL", { meta: true, shift: true }), true, false)).toEqual(LOCAL);
  });
});

describe("shortcutLabels", () => {
  it("names each platform's chords", () => {
    expect(shortcutLabels(true)).toEqual({
      newTab: "⌘T",
      close: "⌘W",
      splitRight: "⌘D",
      splitDown: "⇧⌘D",
      focus: "⌥⌘+Arrows",
      localTerminal: "⇧⌘L",
      zoom: "⇧⌘↩",
      broadcast: "⌥⌘B",
      sidebar: "⌘\\",
    });
    expect(shortcutLabels(false)).toEqual({
      newTab: "Ctrl+Shift+T",
      close: "Ctrl+Shift+W",
      splitRight: "Ctrl+Shift+D",
      splitDown: "Ctrl+Alt+Shift+D",
      focus: "Ctrl+Alt+Arrows",
      localTerminal: "Ctrl+Shift+L",
      zoom: "Ctrl+Shift+Enter",
      broadcast: "Ctrl+Alt+Shift+B",
      sidebar: "Ctrl+Shift+B",
    });
  });
});

describe("isSidebarToggleKey", () => {
  it("is Cmd+\\ on macOS and nothing else there", () => {
    expect(isSidebarToggleKey(key("Backslash", { meta: true }, "\\"), true)).toBe(true);
    expect(isSidebarToggleKey(key("Backslash", {}, "\\"), true)).toBe(false);
    expect(isSidebarToggleKey(key("Backslash", { meta: true, shift: true }, "|"), true)).toBe(false);
    expect(isSidebarToggleKey(key("Backslash", { ctrl: true }, "\\"), true)).toBe(false);
    expect(isSidebarToggleKey(key("KeyB", { ctrl: true, shift: true }, "B"), true)).toBe(false);
  });

  it("is Ctrl+Shift+B on Linux and Windows, and plain Ctrl+B stays with the shell", () => {
    expect(isSidebarToggleKey(key("KeyB", { ctrl: true, shift: true }, "B"), false)).toBe(true);
    expect(isSidebarToggleKey(key("KeyB", { ctrl: true }), false)).toBe(false);
    expect(isSidebarToggleKey(key("KeyB", { ctrl: true, alt: true, shift: true }, "B"), false)).toBe(false);
    expect(isSidebarToggleKey(key("KeyB", { meta: true, shift: true }, "B"), false)).toBe(false);
    expect(isSidebarToggleKey(key("Backslash", { ctrl: true }, "\\"), false)).toBe(false);
  });
});
