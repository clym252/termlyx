/**
 * Windows Terminal style copy/paste keys for the terminal.
 *
 * Windows/Linux: Ctrl+C copies when text is selected (and clears the
 * selection) and otherwise is ^C for the shell; Ctrl+Shift+C copies a
 * selection; Ctrl+V and Ctrl+Shift+V paste. macOS keeps the native Cmd+C/V.
 */

import type { Terminal } from "@xterm/xterm";
import { isPaletteKey } from "../app/hotkeys";
import { PLATFORM, type Platform, usesCmdKey } from "../platform";
import { isSidebarToggleKey, matchShortcut, type KeyLike } from "./shortcuts";

/** The key event plus its type; only a keydown acts (keyup and keypress never do). */
type ClipboardKeyEvent = KeyLike & { type?: string };

export function clipboardAction(e: ClipboardKeyEvent, hasSelection: boolean, platform: Platform = PLATFORM): "copy" | "paste" | null {
  if (usesCmdKey(platform)) return null;
  if (e.type !== undefined && e.type !== "keydown") return null;
  if (!e.ctrlKey || e.metaKey || e.altKey) return null;
  switch (e.key.toLowerCase()) {
    case "c":
      // Without a selection, Ctrl+C (and Ctrl+Shift+C) is left alone: ^C reaches the shell.
      return hasSelection ? "copy" : null;
    case "v":
      return "paste";
    default:
      return null;
  }
}

type KeyTerm = Pick<Terminal, "hasSelection" | "getSelection" | "clearSelection">;

/**
 * xterm's custom key handler: false means xterm must not treat the key as
 * terminal input. The palette chord, workspace shortcuts and the sidebar toggle are acted on by
 * the window handler; copy is done here; paste is handled by attachSafePaste on the paste event, which replaces the browser's own
 * paste event, which xterm turns into (bracketed) paste.
 */
export function terminalKeyHandler(term: KeyTerm, platform: Platform = PLATFORM): (e: KeyboardEvent) => boolean {
  return (e) => {
    if (isPaletteKey(e, true, platform)) return false;
    if (matchShortcut(e, usesCmdKey(platform)) !== null) return false;
    if (isSidebarToggleKey(e, usesCmdKey(platform))) return false;
    switch (clipboardAction(e, term.hasSelection(), platform)) {
      case "copy":
        void navigator.clipboard.writeText(term.getSelection()).catch((err: unknown) => console.debug("copy refused", err));
        term.clearSelection();
        e.preventDefault();
        return false;
      case "paste":
        return false;
      default:
        return true;
    }
  };
}
