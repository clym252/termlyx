import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeyBar } from "./KeyBar";
import { getKeyBarMode, setKeyBarMode } from "./keyBarPref";
import { NO_MODS, type Mods } from "./keyMap";

beforeEach(() => {
  setKeyBarMode("two");
  localStorage.clear();
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

/** Each row's buttons: a terminal key by its `data-key`, any other button by its name. */
const rowsOf = (container: HTMLElement) =>
  [...container.querySelectorAll(".key-row")].map((r) =>
    [...r.querySelectorAll("button")].map((b) => b.dataset.key ?? b.ariaLabel),
  );

/** The bar with its modifiers held in state, as TerminalPane holds them. */
function Bar({ onSend = () => {}, onSnippets = () => {}, onHideKeyboard = () => {}, appCursor = false }) {
  const [mods, setMods] = useState<Mods>(NO_MODS);
  return (
    <KeyBar
      onSend={onSend}
      onSnippets={onSnippets}
      onHideKeyboard={onHideKeyboard}
      appCursor={appCursor}
      mods={mods}
      onModsChange={setMods}
    />
  );
}

const press = (name: string) => {
  const b = screen.getByRole("button", { name });
  fireEvent.pointerDown(b);
  fireEvent.pointerUp(b);
};

describe("KeyBar", () => {
  it("renders_two_rows_with_the_specs_keys_in_order", () => {
    const { container } = render(<Bar />);
    expect(rowsOf(container)).toEqual([
      ["esc", "ctrl", "alt", "tab", "|", "~", "/", "-", "snippets", "One row"],
      ["home", "end", "pgup", "pgdn", "left", "up", "down", "right", "Hide keyboard"],
    ]);
    expect(screen.getByRole("toolbar", { name: "Terminal keys" })).toBeTruthy();
  });

  it("keys_send_their_bytes_on_press", () => {
    const onSend = vi.fn();
    render(<Bar onSend={onSend} />);
    press("Esc");
    press("Tab");
    press("|");
    press("Page Down");
    expect(onSend.mock.calls.map((c) => c[0])).toEqual(["\x1b", "\t", "|", "\x1b[6~"]);
  });

  it("ctrl_once_applies_to_the_next_key_then_releases", () => {
    const onSend = vi.fn();
    render(<Bar onSend={onSend} />);
    const ctrl = screen.getByRole("button", { name: "Ctrl" });
    press("Ctrl");
    expect(ctrl.getAttribute("aria-pressed")).toBe("true");
    expect(ctrl.dataset.mod).toBe("once");
    press("Left");
    press("Left");
    expect(onSend.mock.calls.map((c) => c[0])).toEqual(["\x1b[1;5D", "\x1b[D"]);
    expect(ctrl.getAttribute("aria-pressed")).toBe("false");
  });

  it("ctrl_tapped_twice_locks_and_a_third_tap_releases", () => {
    const onSend = vi.fn();
    render(<Bar onSend={onSend} />);
    const ctrl = screen.getByRole("button", { name: "Ctrl" });
    press("Ctrl");
    press("Ctrl");
    expect(ctrl.dataset.mod).toBe("locked");
    press("Up");
    press("Up");
    expect(onSend.mock.calls.map((c) => c[0])).toEqual(["\x1b[1;5A", "\x1b[1;5A"]);
    press("Ctrl");
    expect(ctrl.dataset.mod).toBe("off");
  });

  it("arrows_follow_application_cursor_mode", () => {
    const onSend = vi.fn();
    render(<Bar onSend={onSend} appCursor />);
    press("Up");
    expect(onSend).toHaveBeenCalledWith("\x1bOA");
  });

  it("arrows_repeat_while_held", () => {
    vi.useFakeTimers();
    const onSend = vi.fn();
    render(<Bar onSend={onSend} />);
    const down = screen.getByRole("button", { name: "Down" });
    fireEvent.pointerDown(down);
    expect(onSend).toHaveBeenCalledTimes(1);
    act(() => vi.advanceTimersByTime(399));
    expect(onSend).toHaveBeenCalledTimes(1);
    act(() => vi.advanceTimersByTime(1));
    expect(onSend).toHaveBeenCalledTimes(2);
    act(() => vi.advanceTimersByTime(120));
    expect(onSend).toHaveBeenCalledTimes(4);
    fireEvent.pointerUp(down);
    act(() => vi.advanceTimersByTime(1000));
    expect(onSend).toHaveBeenCalledTimes(4);
    expect(new Set(onSend.mock.calls.map((c) => c[0]))).toEqual(new Set(["\x1b[B"]));
  });

  it("other_keys_do_not_repeat", () => {
    vi.useFakeTimers();
    const onSend = vi.fn();
    render(<Bar onSend={onSend} />);
    fireEvent.pointerDown(screen.getByRole("button", { name: "Esc" }));
    act(() => vi.advanceTimersByTime(1000));
    expect(onSend).toHaveBeenCalledTimes(1);
  });

  it("snippets_opens_the_sheet_and_sends_nothing", () => {
    const onSend = vi.fn();
    const onSnippets = vi.fn();
    render(<Bar onSend={onSend} onSnippets={onSnippets} />);
    press("Snippets");
    expect(onSnippets).toHaveBeenCalledTimes(1);
    expect(onSend).not.toHaveBeenCalled();
  });

  it("hide_keyboard_asks_to_close_the_keyboard_and_sends_nothing", () => {
    const onSend = vi.fn();
    const onHideKeyboard = vi.fn();
    render(<Bar onSend={onSend} onHideKeyboard={onHideKeyboard} />);
    const hide = screen.getByRole("button", { name: "Hide keyboard" });
    expect(hide.textContent).toBe("⌄");
    press("Hide keyboard");
    expect(onHideKeyboard).toHaveBeenCalledTimes(1);
    expect(onSend).not.toHaveBeenCalled();
  });

  it("hide_keyboard_works_from_assistive_tech", () => {
    const onHideKeyboard = vi.fn();
    render(<Bar onHideKeyboard={onHideKeyboard} />);
    // A click with no press before it (detail 0), as VoiceOver activates.
    fireEvent.click(screen.getByRole("button", { name: "Hide keyboard" }), { detail: 0 });
    expect(onHideKeyboard).toHaveBeenCalledTimes(1);
  });

  it("the_size_toggle_goes_to_one_row_with_the_everyday_keys", () => {
    const onSend = vi.fn();
    const { container } = render(<Bar onSend={onSend} />);
    press("One row");
    expect(rowsOf(container)).toEqual([
      ["esc", "ctrl", "alt", "tab", "left", "up", "down", "right", "Hide key bar", "Hide keyboard"],
    ]);
    expect(onSend).not.toHaveBeenCalled();
    // The keys left work as before.
    press("Esc");
    press("Left");
    expect(onSend.mock.calls.map((c) => c[0])).toEqual(["\x1b", "\x1b[D"]);
  });

  it("then_hides_the_bar_behind_a_pill_that_brings_both_rows_back", () => {
    const { container } = render(<Bar />);
    press("One row");
    press("Hide key bar");
    expect(screen.queryByRole("toolbar", { name: "Terminal keys" })).toBeNull();
    expect(container.querySelectorAll("button.key")).toHaveLength(0);
    const show = screen.getByRole("button", { name: "Show key bar" });
    // The pill does not take the focus either (the keyboard stays up).
    expect(show.tabIndex).toBe(-1);
    expect(fireEvent.pointerDown(show)).toBe(false);
    fireEvent.pointerUp(show);
    expect(rowsOf(container)).toHaveLength(2);
  });

  it("remembers_the_size_on_this_device", () => {
    render(<Bar />);
    press("One row");
    expect(getKeyBarMode()).toBe("one");
    expect(localStorage.getItem("tether.keyBar")).toBe("one");
    cleanup();
    // A bar shown later (another session, the keyboard up again) starts as it was left.
    const { container } = render(<Bar />);
    expect(rowsOf(container)).toHaveLength(1);
  });

  it("hiding_the_bar_lets_go_of_sticky_modifiers", () => {
    const onSend = vi.fn();
    render(<Bar onSend={onSend} />);
    press("Ctrl");
    press("Ctrl");
    expect(screen.getByRole("button", { name: "Ctrl" }).dataset.mod).toBe("locked");
    press("One row");
    // Still on screen in one row: kept.
    expect(screen.getByRole("button", { name: "Ctrl" }).dataset.mod).toBe("locked");
    press("Hide key bar");
    press("Show key bar");
    expect(screen.getByRole("button", { name: "Ctrl" }).dataset.mod).toBe("off");
  });

  it("the_size_toggle_works_from_assistive_tech", () => {
    const { container } = render(<Bar />);
    fireEvent.click(screen.getByRole("button", { name: "One row" }), { detail: 0 });
    expect(rowsOf(container)).toHaveLength(1);
  });

  it("a_press_keeps_the_focus_where_it_is", () => {
    render(<Bar />);
    const esc = screen.getByRole("button", { name: "Esc" });
    // Not a tab stop, and the press itself does not move the focus (the soft keyboard stays up).
    expect(esc.tabIndex).toBe(-1);
    expect(fireEvent.pointerDown(esc)).toBe(false);
    expect(fireEvent.mouseDown(esc)).toBe(false);
  });
});
