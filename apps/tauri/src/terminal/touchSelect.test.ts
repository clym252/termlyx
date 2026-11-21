import { describe, expect, it, vi } from "vitest";
import { LONG_PRESS_MS, SLOP_PX } from "./touchScroll";
import {
  LongPress,
  MENU_CLOSED,
  MENU_GAP,
  boundaryAt,
  cellAt,
  dragHandle,
  edgeDirection,
  handlePoints,
  menuItems,
  menuReducer,
  placeMenu,
  wordRange,
  type Grid,
  type MenuState,
  type RowCells,
  type Timers,
} from "./touchSelect";

/** 80×24 cells of 10×20 px, drawn at (5, 50), scrolled so buffer line 100 is on top. */
const G: Grid = { left: 5, top: 50, width: 800, height: 480, cols: 80, rows: 24, viewportY: 100 };

const cells = (s: string): RowCells => [...s].map((c) => (c === "\0" ? null : c));

describe("cellAt / boundaryAt", () => {
  it("maps_a_point_to_the_cell_under_it_in_buffer_rows", () => {
    expect(cellAt(G, 5, 50)).toEqual({ col: 0, row: 100 });
    expect(cellAt(G, 5 + 10 * 7 + 9.9, 50 + 20 * 3 + 1)).toEqual({ col: 7, row: 103 });
    expect(cellAt(G, 804.9, 529.9)).toEqual({ col: 79, row: 123 });
  });

  it("clamps_points_off_the_screen_to_its_edge_cells", () => {
    expect(cellAt(G, -40, 10)).toEqual({ col: 0, row: 100 });
    expect(cellAt(G, 2000, 900)).toEqual({ col: 79, row: 123 });
  });

  it("boundary_is_the_nearest_cell_edge_up_to_the_end_of_the_row", () => {
    expect(boundaryAt(G, 5 + 10 * 3 + 4, 60)).toEqual({ col: 3, row: 100 });
    expect(boundaryAt(G, 5 + 10 * 3 + 6, 60)).toEqual({ col: 4, row: 100 });
    expect(boundaryAt(G, 900, 60)).toEqual({ col: 80, row: 100 });
  });
});

describe("wordRange", () => {
  it("selects_the_run_of_word_characters_under_the_finger", () => {
    const row = cells("ls -la /var/log/syslog.1  ");
    expect(wordRange(row, 0)).toEqual({ start: 0, end: 2 });
    expect(wordRange(row, 4)).toEqual({ start: 3, end: 6 });
    // Paths stay whole (only xterm's separators split words).
    expect(wordRange(row, 12)).toEqual({ start: 7, end: 24 });
  });

  it("stops_at_separators_and_selects_one_blank_or_separator_alone", () => {
    const row = cells("echo 'a(b)c' x");
    expect(wordRange(row, 6)).toEqual({ start: 6, end: 7 });
    expect(wordRange(row, 5)).toEqual({ start: 5, end: 6 });
    expect(wordRange(row, 4)).toEqual({ start: 4, end: 5 });
    expect(wordRange(row, 13)).toEqual({ start: 13, end: 14 });
  });

  it("keeps_wide_characters_whole", () => {
    // "日本" is two wide characters (four cells), then " x".
    const row = cells("日\0本\0 x");
    expect(wordRange(row, 3)).toEqual({ start: 0, end: 4 });
    expect(wordRange(row, 1)).toEqual({ start: 0, end: 4 });
  });

  it("clamps_a_column_past_the_row", () => {
    expect(wordRange(cells("abc"), 9)).toEqual({ start: 0, end: 3 });
  });
});

describe("dragHandle", () => {
  const r = { start: { col: 10, row: 5 }, end: { col: 20, row: 5 } };

  it("moves_the_dragged_end_across_rows", () => {
    expect(dragHandle(r, "start", { col: 3, row: 4 }, 80)).toEqual({ start: { col: 3, row: 4 }, end: r.end });
    expect(dragHandle(r, "end", { col: 2, row: 7 }, 80)).toEqual({ start: r.start, end: { col: 2, row: 7 } });
  });

  it("an_end_at_the_end_of_a_row_is_the_start_of_the_next", () => {
    expect(dragHandle(r, "end", { col: 80, row: 5 }, 80).end).toEqual({ col: 0, row: 6 });
  });

  it("a_handle_never_passes_the_other_one", () => {
    expect(dragHandle(r, "start", { col: 40, row: 6 }, 80)).toEqual({ start: { col: 19, row: 5 }, end: r.end });
    expect(dragHandle(r, "end", { col: 0, row: 0 }, 80)).toEqual({ start: r.start, end: { col: 11, row: 5 } });
  });
});

describe("handlePoints / edgeDirection", () => {
  it("puts_the_handles_before_the_first_cell_and_after_the_last", () => {
    const h = handlePoints(G, { start: { col: 4, row: 102 }, end: { col: 0, row: 104 } });
    // Start: before column 4 of the screen's third row.
    expect(h.start).toEqual({ x: 5 + 40, top: 50 + 40, bottom: 50 + 60, visible: true });
    // End: an end at the start of a row is drawn after the last cell of the row before.
    expect(h.end).toEqual({ x: 5 + 800, top: 50 + 60, bottom: 50 + 80, visible: true });
  });

  it("an_end_scrolled_off_the_screen_is_not_visible", () => {
    const h = handlePoints(G, { start: { col: 0, row: 90 }, end: { col: 5, row: 130 } });
    expect(h.start.visible).toBe(false);
    expect(h.end.visible).toBe(false);
  });

  it("past_the_top_scrolls_back_past_the_bottom_forward", () => {
    expect(edgeDirection(G, 49)).toBe(-1);
    expect(edgeDirection(G, 50)).toBe(0);
    expect(edgeDirection(G, 530)).toBe(0);
    expect(edgeDirection(G, 531)).toBe(1);
  });
});

describe("placeMenu", () => {
  const bounds = { left: 0, top: 0, right: 400, bottom: 700 };
  const size = { w: 200, h: 46 };

  it("sits_centred_above_its_anchor", () => {
    expect(placeMenu({ x: 200, top: 300, bottom: 340 }, size, bounds)).toEqual({ x: 100, y: 300 - MENU_GAP - 46 });
  });

  it("goes_below_without_room_above_and_stays_inside", () => {
    expect(placeMenu({ x: 10, top: 20, bottom: 60 }, size, bounds)).toEqual({ x: MENU_GAP, y: 60 + MENU_GAP });
    expect(placeMenu({ x: 395, top: 5, bottom: 690 }, size, bounds)).toEqual({
      x: 400 - MENU_GAP - 200,
      y: 700 - MENU_GAP - 46,
    });
  });
});

describe("menu state machine", () => {
  const press = { type: "press", x: 30, y: 40, cell: { col: 2, row: 7 } } as const;

  it("a_long_press_offers_paste_select_and_select_all", () => {
    const s = menuReducer(MENU_CLOSED, press);
    expect(s).toEqual({ kind: "actions", x: 30, y: 40, cell: { col: 2, row: 7 } });
    expect(menuItems(s)).toEqual(["Paste", "Select", "Select All"]);
  });

  it("with_a_selection_it_offers_copy_save_select_all_and_done_hidden_while_a_handle_moves", () => {
    let s: MenuState = menuReducer(menuReducer(MENU_CLOSED, press), { type: "selected" });
    expect(menuItems(s)).toEqual(["Copy", "Save as snippet", "Select All", "Done"]);
    s = menuReducer(s, { type: "dragStart" });
    expect(menuItems(s)).toEqual([]);
    s = menuReducer(s, { type: "dragEnd" });
    expect(menuItems(s)).toEqual(["Copy", "Save as snippet", "Select All", "Done"]);
  });

  it("a_tap_or_an_action_closes_it", () => {
    const actions = menuReducer(MENU_CLOSED, press);
    expect(menuReducer(actions, { type: "tap" })).toBe(MENU_CLOSED);
    expect(menuReducer(menuReducer(actions, { type: "selected" }), { type: "close" })).toBe(MENU_CLOSED);
    expect(menuItems(MENU_CLOSED)).toEqual([]);
  });

  it("a_long_press_with_a_selection_starts_over_at_the_finger", () => {
    const sel = menuReducer(menuReducer(MENU_CLOSED, press), { type: "selected" });
    expect(menuReducer(sel, { ...press, x: 99 }).kind).toBe("actions");
  });

  it("drag_events_without_a_selection_change_nothing", () => {
    const actions = menuReducer(MENU_CLOSED, press);
    expect(menuReducer(actions, { type: "dragStart" })).toBe(actions);
    expect(menuReducer(MENU_CLOSED, { type: "dragEnd" })).toBe(MENU_CLOSED);
  });
});

/** Timers run by hand: `run()` fires the pending one. */
function timers() {
  let pending: (() => void) | null = null;
  const t: Timers & { run(): void; pending(): boolean } = {
    set(cb) {
      pending = cb;
      return 1;
    },
    clear() {
      pending = null;
    },
    run() {
      const cb = pending;
      pending = null;
      cb?.();
    },
    pending: () => pending !== null,
  };
  return t;
}

describe("LongPress", () => {
  it("a_finger_held_still_fires_a_press_at_where_it_went_down", () => {
    const onPress = vi.fn();
    const t = timers();
    const lp = new LongPress(onPress, () => false, t);
    lp.start(10, 20);
    expect(lp.move(10 + SLOP_PX - 1, 20)).toBe(false);
    t.run();
    expect(onPress).toHaveBeenCalledWith(10, 20);
    // After the press the finger's moves and lift are its own: no scroll, no click.
    expect(lp.move(80, 90)).toBe(true);
    expect(lp.end()).toBe(true);
  });

  it("uses_the_same_hold_as_touch_scroll", () => {
    const set = vi.fn(() => 1);
    new LongPress(vi.fn(), () => false, { set, clear: vi.fn() }).start(0, 0);
    expect(set).toHaveBeenCalledWith(expect.any(Function), LONG_PRESS_MS);
  });

  it("a_move_past_the_slop_is_a_scroll_neither_press_nor_tap", () => {
    const onPress = vi.fn();
    const onTap = vi.fn(() => true);
    const t = timers();
    const lp = new LongPress(onPress, onTap, t);
    lp.start(10, 20);
    lp.move(10, 20 + SLOP_PX);
    expect(t.pending()).toBe(false);
    expect(lp.end()).toBe(false);
    expect(onPress).not.toHaveBeenCalled();
    expect(onTap).not.toHaveBeenCalled();
  });

  it("a_quick_lift_is_a_tap_that_swallows_the_click_only_when_it_closed_something", () => {
    let open = true;
    const onTap = vi.fn(() => {
      const was = open;
      open = false;
      return was;
    });
    const t = timers();
    const lp = new LongPress(vi.fn(), onTap, t);
    lp.start(1, 1);
    expect(lp.end()).toBe(true);
    lp.start(1, 1);
    expect(lp.end()).toBe(false);
    expect(onTap).toHaveBeenCalledTimes(2);
    expect(t.pending()).toBe(false);
  });

  it("cancel_drops_the_pending_press", () => {
    const onPress = vi.fn();
    const t = timers();
    const lp = new LongPress(onPress, () => false, t);
    lp.start(1, 1);
    lp.cancel();
    t.run();
    expect(onPress).not.toHaveBeenCalled();
    expect(lp.end()).toBe(false);
  });
});
