/**
 * Workspace shortcuts (ruling R30).
 *
 * macOS: Cmd+T new tab, Cmd+W close pane, Cmd+D split right, Shift+Cmd+D
 * split down, Cmd+Alt+Arrows focus, Shift+Cmd+L local terminal, Shift+Cmd+Enter
 * zoom the pane, Cmd+Alt+B broadcast typing to the tab's panes.
 *
 * Linux/Windows: Ctrl+Shift+T, Ctrl+Shift+W, Ctrl+Shift+D, Ctrl+Alt+Shift+D
 * (split down), Ctrl+Alt+Arrows, Ctrl+Shift+L, Ctrl+Shift+Enter (zoom),
 * Ctrl+Alt+Shift+B (broadcast). Plain Ctrl+letter is never a workspace
 * shortcut there, so Ctrl+T/W/D/B (transpose, delete word, EOF, back-one-char)
 * reach the shell. Broadcast is not on I: Cmd+Alt+I opens the web inspector.
 *
 * iOS (an iPad with a hardware keyboard) uses all the Mac chords except the
 * local terminal (there is none), so no Shift+Cmd+L.
 *
 * Sidebar toggle (not a workspace command, so not in `matchShortcut`): Cmd+\
 * on macOS, Ctrl+Shift+B on Linux/Windows (plain Ctrl+B is tmux's prefix and
 * readline's back-one-char).
 */

import { CMD_KEYS, IS_MOBILE } from "../platform";

export type WorkspaceCommand =
  | { kind: "newTab" }
  | { kind: "close" }
  | { kind: "split"; orientation: "horizontal" | "vertical" }
  | { kind: "focus"; direction: "left" | "right" | "up" | "down" }
  | { kind: "localTerminal" }
  | { kind: "zoom" }
  | { kind: "broadcast" };

export type KeyLike = Pick<KeyboardEvent, "key" | "code" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey">;

const ARROWS: Record<string, "left" | "right" | "up" | "down"> = {
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
};

/** The workspace command a key event means, if any. */
export function matchShortcut(e: KeyLike, mac = CMD_KEYS, mobile = IS_MOBILE): WorkspaceCommand | null {
  const cmd = mac ? matchMac(e) : matchOther(e);
  // There is no local terminal on iOS or Android.
  if (mobile && cmd?.kind === "localTerminal") return null;
  return cmd;
}

function matchMac(e: KeyLike): WorkspaceCommand | null {
  if (!e.metaKey || e.ctrlKey) return null;
  if (e.altKey) {
    if (e.code === "KeyB") return e.shiftKey ? null : { kind: "broadcast" };
    const direction = ARROWS[e.key];
    return direction && !e.shiftKey ? { kind: "focus", direction } : null;
  }
  // `code` so Shift (and keyboard layouts that move letters) do not matter.
  switch (e.code) {
    case "KeyT":
      return e.shiftKey ? null : { kind: "newTab" };
    case "KeyW":
      return e.shiftKey ? null : { kind: "close" };
    case "KeyD":
      return { kind: "split", orientation: e.shiftKey ? "vertical" : "horizontal" };
    case "KeyL":
      return e.shiftKey ? { kind: "localTerminal" } : null;
    case "Enter":
      return e.shiftKey ? { kind: "zoom" } : null;
    default:
      return null;
  }
}

function matchOther(e: KeyLike): WorkspaceCommand | null {
  if (!e.ctrlKey || e.metaKey) return null;
  if (e.altKey && !e.shiftKey) {
    const direction = ARROWS[e.key];
    return direction ? { kind: "focus", direction } : null;
  }
  // Everything else needs Shift, so plain Ctrl+letter stays with the shell.
  if (!e.shiftKey) return null;
  if (e.altKey) {
    if (e.code === "KeyD") return { kind: "split", orientation: "vertical" };
    return e.code === "KeyB" ? { kind: "broadcast" } : null;
  }
  switch (e.code) {
    case "KeyT":
      return { kind: "newTab" };
    case "KeyW":
      return { kind: "close" };
    case "KeyD":
      return { kind: "split", orientation: "horizontal" };
    case "KeyL":
      return { kind: "localTerminal" };
    case "Enter":
      return { kind: "zoom" };
    default:
      return null;
  }
}

/** Whether a key event cycles the sidebar: full, icon rail, hidden. */
export function isSidebarToggleKey(e: KeyLike, mac = CMD_KEYS): boolean {
  if (e.altKey) return false;
  if (mac) return e.metaKey && !e.ctrlKey && !e.shiftKey && e.code === "Backslash";
  return e.ctrlKey && e.shiftKey && !e.metaKey && e.code === "KeyB";
}

export type ShortcutLabels = {
  newTab: string;
  close: string;
  splitRight: string;
  splitDown: string;
  focus: string;
  localTerminal: string;
  zoom: string;
  broadcast: string;
  sidebar: string;
};

/** Labels for menus and tooltips on a platform. */
export function shortcutLabels(mac: boolean): ShortcutLabels {
  return mac
    ? {
        newTab: "⌘T",
        close: "⌘W",
        splitRight: "⌘D",
        splitDown: "⇧⌘D",
        focus: "⌥⌘+Arrows",
        localTerminal: "⇧⌘L",
        zoom: "⇧⌘↩",
        broadcast: "⌥⌘B",
        sidebar: "⌘\\",
      }
    : {
        newTab: "Ctrl+Shift+T",
        close: "Ctrl+Shift+W",
        splitRight: "Ctrl+Shift+D",
        splitDown: "Ctrl+Alt+Shift+D",
        focus: "Ctrl+Alt+Arrows",
        localTerminal: "Ctrl+Shift+L",
        zoom: "Ctrl+Shift+Enter",
        broadcast: "Ctrl+Alt+Shift+B",
        sidebar: "Ctrl+Shift+B",
      };
}

/** Labels for this platform. */
export const SHORTCUT_LABELS = shortcutLabels(CMD_KEYS);
