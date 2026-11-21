/**
 * The iOS key bar's keys and the bytes they send (pure; `KeyBar` draws them).
 *
 * `ctrl` and `alt` are sticky: a tap applies them to the next key, from the
 * bar or the software keyboard ("once"), a second tap locks them, a third
 * releases them.
 */

export const KEY_ROWS = [
  ["esc", "ctrl", "alt", "tab", "|", "~", "/", "-", "snippets"],
  ["home", "end", "pgup", "pgdn", "left", "up", "down", "right"],
] as const;

export type BarKey = (typeof KEY_ROWS)[number][number];

/** The key bar's one-row mode: the keys used most. */
export const COMPACT_ROW: readonly BarKey[] = ["esc", "ctrl", "alt", "tab", "left", "up", "down", "right"];

export type ModState = "off" | "once" | "locked";
export type Mods = { ctrl: ModState; alt: ModState };

export const NO_MODS: Mods = { ctrl: "off", alt: "off" };

const ESC = "\x1b";
/** Cursor keys and their final byte (`CSI <final>`; `SS3 <final>` for arrows in application cursor mode). */
const CURSOR: Record<string, string> = { up: "A", down: "B", right: "C", left: "D", home: "H", end: "F" };
const ARROWS = new Set(["up", "down", "right", "left"]);
/** `CSI <n> ~` keys. */
const TILDE: Record<string, string> = { pgup: "5", pgdn: "6" };
/** Ctrl with these makes the control character xterm sends for them. */
const CTRL_SPECIAL: Record<string, string> = { " ": "\x00", "/": "\x1f", "?": "\x7f" };

/** Ctrl applied to one character, or `null` when it has no control form. */
function ctrlChar(ch: string): string | null {
  if (ch in CTRL_SPECIAL) return CTRL_SPECIAL[ch];
  const code = ch.charCodeAt(0);
  // @ A-Z [ \ ] ^ _ and a-z.
  if (ch.length === 1 && ((code >= 0x40 && code <= 0x5f) || (code >= 0x61 && code <= 0x7a))) {
    return String.fromCharCode(code & 0x1f);
  }
  return null;
}

/**
 * The bytes for a bar key (`esc`, `tab`, `home`, `left`, ...) or one typed
 * character, under the modifiers. Cursor keys with a modifier use xterm's
 * parameter form (`CSI 1;5D` for Ctrl+Left); otherwise ctrl makes a control
 * character and alt prefixes ESC.
 */
export function keyBytes(key: string, mods: Mods, appCursor = false): string {
  const ctrl = mods.ctrl !== "off";
  const alt = mods.alt !== "off";
  const param = 1 + (alt ? 2 : 0) + (ctrl ? 4 : 0);
  if (key in CURSOR) {
    const final = CURSOR[key];
    if (param > 1) return `${ESC}[1;${param}${final}`;
    return appCursor && ARROWS.has(key) ? `${ESC}O${final}` : `${ESC}[${final}`;
  }
  if (key in TILDE) return param > 1 ? `${ESC}[${TILDE[key]};${param}~` : `${ESC}[${TILDE[key]}~`;
  let bytes = key === "esc" ? ESC : key === "tab" ? "\t" : key;
  if (ctrl) bytes = ctrlChar(bytes) ?? bytes;
  return alt ? ESC + bytes : bytes;
}

/** A tap on `ctrl` or `alt`: off → once → locked → off. */
export function nextMods(m: Mods, tapped: "ctrl" | "alt"): Mods {
  const next: Record<ModState, ModState> = { off: "once", once: "locked", locked: "off" };
  return { ...m, [tapped]: next[m[tapped]] };
}

/** After any key: a "once" modifier is used up; a locked one stays. */
export function consumeOnce(m: Mods): Mods {
  if (m.ctrl !== "once" && m.alt !== "once") return m;
  return { ctrl: m.ctrl === "once" ? "off" : m.ctrl, alt: m.alt === "once" ? "off" : m.alt };
}

/**
 * What input typed on the software keyboard becomes under the bar's
 * modifiers. One character takes them; anything longer (a word from
 * autocomplete or the IME, a paste, an escape sequence) passes as it is.
 * Either way a "once" modifier is used up.
 */
export function applyTyped(data: string, mods: Mods, appCursor: boolean): { bytes: string; mods: Mods } {
  if (mods.ctrl === "off" && mods.alt === "off") return { bytes: data, mods };
  const bytes = [...data].length === 1 ? keyBytes(data, mods, appCursor) : data;
  return { bytes, mods: consumeOnce(mods) };
}
