import { PLATFORM, type Platform, usesCmdKey } from "../platform";
import type { KeyLike } from "../workspace/shortcuts";

/**
 * Whether a key event opens the command palette.
 *
 * macOS and iOS (hardware keyboard): Cmd+K. Windows/Linux: Ctrl+Shift+K anywhere, and plain Ctrl+K only
 * outside a terminal (inside one it is kill-to-end-of-line and belongs to the
 * shell).
 */
export function isPaletteKey(e: KeyLike, inTerminal: boolean, platform: Platform = PLATFORM): boolean {
  if (e.altKey || e.key.toLowerCase() !== "k") return false;
  if (usesCmdKey(platform)) return e.metaKey && !e.ctrlKey && !e.shiftKey;
  if (e.metaKey || !e.ctrlKey) return false;
  return e.shiftKey || !inTerminal;
}

/** The command palette's chord as shown in the UI. */
export function paletteLabel(platform: Platform = PLATFORM): string {
  return usesCmdKey(platform) ? "⌘K" : "Ctrl+Shift+K";
}

/** True when a key event's target is inside a terminal (xterm's DOM). */
export function isInTerminal(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(".xterm") !== null;
}
