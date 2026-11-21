import { describe, expect, it } from "vitest";
import { KEY_ROWS, NO_MODS, applyTyped, consumeOnce, keyBytes, nextMods, type Mods } from "./keyMap";

const ctrl = (s: Mods["ctrl"]): Mods => ({ ...NO_MODS, ctrl: s });
const alt = (s: Mods["alt"]): Mods => ({ ...NO_MODS, alt: s });

describe("keyBytes", () => {
  it("rows_are_the_specs_keys_in_order", () => {
    expect(KEY_ROWS).toEqual([
      ["esc", "ctrl", "alt", "tab", "|", "~", "/", "-", "snippets"],
      ["home", "end", "pgup", "pgdn", "left", "up", "down", "right"],
    ]);
  });

  it("each_key_sends_its_bytes", () => {
    const sent = (k: string) => keyBytes(k, NO_MODS, false);
    expect(sent("esc")).toBe("\x1b");
    expect(sent("tab")).toBe("\t");
    expect(sent("|")).toBe("|");
    expect(sent("~")).toBe("~");
    expect(sent("/")).toBe("/");
    expect(sent("-")).toBe("-");
    expect(sent("home")).toBe("\x1b[H");
    expect(sent("end")).toBe("\x1b[F");
    expect(sent("pgup")).toBe("\x1b[5~");
    expect(sent("pgdn")).toBe("\x1b[6~");
    expect(sent("up")).toBe("\x1b[A");
    expect(sent("down")).toBe("\x1b[B");
    expect(sent("right")).toBe("\x1b[C");
    expect(sent("left")).toBe("\x1b[D");
  });

  it("arrows_in_application_cursor_mode", () => {
    expect(keyBytes("up", NO_MODS, true)).toBe("\x1bOA");
    expect(keyBytes("down", NO_MODS, true)).toBe("\x1bOB");
    expect(keyBytes("right", NO_MODS, true)).toBe("\x1bOC");
    expect(keyBytes("left", NO_MODS, true)).toBe("\x1bOD");
    // Not the other cursor keys' business.
    expect(keyBytes("home", NO_MODS, true)).toBe("\x1b[H");
  });

  it("ctrl_makes_control_characters", () => {
    expect(keyBytes("c", ctrl("once"), false)).toBe("\x03");
    expect(keyBytes("C", ctrl("once"), false)).toBe("\x03");
    expect(keyBytes("a", ctrl("locked"), false)).toBe("\x01");
    expect(keyBytes("[", ctrl("once"), false)).toBe("\x1b");
    expect(keyBytes(" ", ctrl("once"), false)).toBe("\x00");
    expect(keyBytes("/", ctrl("once"), false)).toBe("\x1f");
    expect(keyBytes("?", ctrl("once"), false)).toBe("\x7f");
    // No control form: unchanged.
    expect(keyBytes("|", ctrl("once"), false)).toBe("|");
    expect(keyBytes("esc", ctrl("once"), false)).toBe("\x1b");
  });

  it("alt_prefixes_escape", () => {
    expect(keyBytes("b", alt("once"), false)).toBe("\x1bb");
    expect(keyBytes("\x7f", alt("once"), false)).toBe("\x1b\x7f");
    expect(keyBytes("c", { ctrl: "once", alt: "once" }, false)).toBe("\x1b\x03");
  });

  it("modified_cursor_keys_use_the_xterm_parameter", () => {
    expect(keyBytes("left", ctrl("once"), false)).toBe("\x1b[1;5D");
    expect(keyBytes("right", alt("once"), true)).toBe("\x1b[1;3C");
    expect(keyBytes("home", { ctrl: "once", alt: "once" }, false)).toBe("\x1b[1;7H");
    expect(keyBytes("pgup", ctrl("locked"), false)).toBe("\x1b[5;5~");
  });
});

describe("sticky modifiers", () => {
  it("next_mods_cycles_off_once_locked_off", () => {
    let m = NO_MODS;
    m = nextMods(m, "ctrl");
    expect(m).toEqual({ ctrl: "once", alt: "off" });
    m = nextMods(m, "ctrl");
    expect(m).toEqual({ ctrl: "locked", alt: "off" });
    m = nextMods(m, "ctrl");
    expect(m).toEqual(NO_MODS);
    expect(nextMods(ctrl("locked"), "alt")).toEqual({ ctrl: "locked", alt: "once" });
  });

  it("once_is_used_up_and_locked_stays", () => {
    expect(consumeOnce({ ctrl: "once", alt: "once" })).toEqual(NO_MODS);
    expect(consumeOnce({ ctrl: "locked", alt: "once" })).toEqual({ ctrl: "locked", alt: "off" });
    const m = NO_MODS;
    expect(consumeOnce(m)).toBe(m);
  });

  it("typed_key_takes_ctrl_once_then_mods_are_off", () => {
    const first = applyTyped("c", ctrl("once"), false);
    expect(first).toEqual({ bytes: "\x03", mods: NO_MODS });
    expect(applyTyped("c", first.mods, false)).toEqual({ bytes: "c", mods: NO_MODS });
  });

  it("typed_key_keeps_a_locked_ctrl", () => {
    expect(applyTyped("d", ctrl("locked"), false)).toEqual({ bytes: "\x04", mods: ctrl("locked") });
  });

  it("typed_text_or_sequences_pass_unchanged_but_use_up_once", () => {
    expect(applyTyped("hello", ctrl("once"), false)).toEqual({ bytes: "hello", mods: NO_MODS });
    expect(applyTyped("\x1b[A", alt("once"), false)).toEqual({ bytes: "\x1b[A", mods: NO_MODS });
  });

  it("typed_input_without_mods_is_untouched", () => {
    const r = applyTyped("ls\r", NO_MODS, false);
    expect(r.bytes).toBe("ls\r");
    expect(r.mods).toBe(NO_MODS);
  });
});
