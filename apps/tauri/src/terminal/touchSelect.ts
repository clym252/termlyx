import type { Terminal } from "@xterm/xterm";
import { LONG_PRESS_MS, SLOP_PX } from "./touchScroll";

/**
 * Touch copy and paste for the terminal (iOS): the gesture and geometry
 * logic, free of React. `TouchSelection.tsx` draws the menu and handles.
 *
 * - holding a finger still for `LONG_PRESS_MS` opens a small menu at the
 *   finger: Paste, Select (the word under the finger), Select All;
 * - with a selection, two handles mark its ends and drag to adjust it
 *   (dragging past the top or bottom edge of the screen scrolls), and the
 *   menu offers Copy, Select All and Done;
 * - a tap closes the menu (and drops the selection) without bringing up the
 *   keyboard; a drag that is not on a handle still scrolls (touchScroll.ts).
 *
 * Positions are buffer cells: a column (0-based) and an absolute buffer row,
 * so a selection stays on its text while the view scrolls.
 */

/** A cell, or the boundary before it: `col` 0-based (a boundary may be `cols`, the end of the row), `row` an absolute buffer line. */
export interface Cell {
  col: number;
  row: number;
}

/** A selection: from `start` (inclusive) to `end` (exclusive), never empty. */
export interface Range {
  start: Cell;
  end: Cell;
}

/** The drawn screen: its box (client px), its size in cells, and the buffer line on its top row. */
export interface Grid {
  left: number;
  top: number;
  width: number;
  height: number;
  cols: number;
  rows: number;
  viewportY: number;
}

/** xterm's default word separators (`wordSeparator`): a double-tap there and Select here pick the same words. */
export const WORD_SEPARATORS = " ()[]{}',\"`";

const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

/** A position as one number: cells from the buffer's start, row by row. */
export const indexOf = (c: Cell, cols: number) => c.row * cols + c.col;
export const cellOf = (i: number, cols: number): Cell => ({ col: i % cols, row: Math.floor(i / cols) });

/** The cell under a client point, clamped to the screen. */
export function cellAt(g: Grid, x: number, y: number): Cell {
  const cw = g.width / g.cols;
  const ch = g.height / g.rows;
  return {
    col: clamp(Math.floor((x - g.left) / cw), 0, g.cols - 1),
    row: g.viewportY + clamp(Math.floor((y - g.top) / ch), 0, g.rows - 1),
  };
}

/** The cell boundary nearest a client point (where a handle lands), clamped to the screen. */
export function boundaryAt(g: Grid, x: number, y: number): Cell {
  const cw = g.width / g.cols;
  const ch = g.height / g.rows;
  return {
    col: clamp(Math.round((x - g.left) / cw), 0, g.cols),
    row: g.viewportY + clamp(Math.floor((y - g.top) / ch), 0, g.rows - 1),
  };
}

/**
 * One row's cells as characters: `" "` for a blank cell, `null` for the
 * second half of a wide character.
 */
export type RowCells = (string | null)[];

type Kind = "word" | "space" | "separator";

function kindAt(cells: RowCells, i: number): Kind {
  // The second half of a wide character is that character.
  while (i > 0 && cells[i] === null) i--;
  const c = cells[i] ?? " ";
  if (c === " " || c === "\t") return "space";
  return WORD_SEPARATORS.includes(c) ? "separator" : "word";
}

/**
 * The word at `col` in a row, as columns `[start, end)`: a run of characters
 * that are neither blank nor separators. On a blank or a separator, that one
 * character. Words do not continue onto a wrapped row.
 */
export function wordRange(cells: RowCells, col: number): { start: number; end: number } {
  let c = clamp(col, 0, cells.length - 1);
  while (c > 0 && cells[c] === null) c--;
  let start = c;
  let end = c + 1;
  if (kindAt(cells, c) === "word") {
    while (start > 0 && kindAt(cells, start - 1) === "word") start--;
    while (end < cells.length && kindAt(cells, end) === "word") end++;
  }
  // Take a wide character's second half along.
  while (end < cells.length && cells[end] === null) end++;
  return { start, end };
}

export type HandleKind = "start" | "end";

/**
 * A handle dragged to boundary `to`: the new selection. A handle cannot pass
 * the other one (at least one cell stays selected).
 */
export function dragHandle(r: Range, which: HandleKind, to: Cell, cols: number): Range {
  const s = indexOf(r.start, cols);
  const e = indexOf(r.end, cols);
  const t = indexOf(to, cols);
  return which === "start"
    ? { start: cellOf(Math.min(t, e - 1), cols), end: r.end }
    : { start: r.start, end: cellOf(Math.max(t, s + 1), cols) };
}

/**
 * Where a dragged handle's point is past an edge of the screen: -1 above the
 * top (the view scrolls back), 1 below the bottom, 0 on the screen.
 */
export function edgeDirection(g: Grid, y: number): -1 | 0 | 1 {
  if (y < g.top) return -1;
  if (y > g.top + g.height) return 1;
  return 0;
}

/** A selection end on screen (client px): the boundary's x and its row's top and bottom; `visible` false off screen. */
export interface HandlePoint {
  x: number;
  top: number;
  bottom: number;
  visible: boolean;
}

/** Where a selection's two handles go: before its first cell and after its last. */
export function handlePoints(g: Grid, r: Range): { start: HandlePoint; end: HandlePoint } {
  const cw = g.width / g.cols;
  const ch = g.height / g.rows;
  const point = (row: number, col: number): HandlePoint => {
    const onScreen = row - g.viewportY;
    const top = g.top + onScreen * ch;
    return { x: g.left + col * cw, top, bottom: top + ch, visible: onScreen >= 0 && onScreen < g.rows };
  };
  // The end handle sits after the last selected cell (on its row, not at the start of the next).
  const last = cellOf(indexOf(r.end, g.cols) - 1, g.cols);
  return { start: point(r.start.row, r.start.col), end: point(last.row, last.col + 1) };
}

/** A box to keep the menu in (px, same frame as the anchor). */
export interface Bounds {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** Space kept between the menu and its anchor, and between it and the edges (px). */
export const MENU_GAP = 10;

/**
 * Top-left of a `w`×`h` menu over what it is for (`x`, from `top` to
 * `bottom`): centred above it, or below when there is no room above, kept
 * inside `bounds`.
 */
export function placeMenu(
  anchor: { x: number; top: number; bottom: number },
  size: { w: number; h: number },
  bounds: Bounds,
): { x: number; y: number } {
  let y = anchor.top - MENU_GAP - size.h;
  if (y < bounds.top + MENU_GAP) y = anchor.bottom + MENU_GAP;
  y = clamp(y, bounds.top + MENU_GAP, Math.max(bounds.top + MENU_GAP, bounds.bottom - MENU_GAP - size.h));
  const x = clamp(
    anchor.x - size.w / 2,
    bounds.left + MENU_GAP,
    Math.max(bounds.left + MENU_GAP, bounds.right - MENU_GAP - size.w),
  );
  return { x, y };
}

/** The menu: closed, the actions at a long press, or the selection's. */
export type MenuState =
  | { kind: "closed" }
  /** Paste / Select / Select All, at the finger (`x`, `y`: client px; `cell` under it). */
  | { kind: "actions"; x: number; y: number; cell: Cell }
  /** Copy / Select All / Done over the selection; hidden while a handle is dragged. */
  | { kind: "selection"; dragging: boolean };

export type MenuEvent =
  /** A long press: the actions at the finger (any selection is dropped first). */
  | { type: "press"; x: number; y: number; cell: Cell }
  /** A tap on the terminal: everything closes (the caller drops the selection). */
  | { type: "tap" }
  /** Select or Select All made a selection. */
  | { type: "selected" }
  | { type: "dragStart" }
  | { type: "dragEnd" }
  /** Paste, Copy, Done, or the selection went away (new output, the screen switched). */
  | { type: "close" };

export const MENU_CLOSED: MenuState = { kind: "closed" };

export function menuReducer(state: MenuState, ev: MenuEvent): MenuState {
  switch (ev.type) {
    case "press":
      return { kind: "actions", x: ev.x, y: ev.y, cell: ev.cell };
    case "selected":
      return { kind: "selection", dragging: false };
    case "dragStart":
    case "dragEnd": {
      const dragging = ev.type === "dragStart";
      return state.kind === "selection" && state.dragging !== dragging ? { kind: "selection", dragging } : state;
    }
    case "tap":
    case "close":
      return state.kind === "closed" ? state : MENU_CLOSED;
  }
}

export type MenuItem = "Paste" | "Select" | "Select All" | "Copy" | "Save as snippet" | "Done";

/** The menu's items, in order (none while closed or while a handle is dragged). */
export function menuItems(state: MenuState): MenuItem[] {
  if (state.kind === "actions") return ["Paste", "Select", "Select All"];
  if (state.kind === "selection" && !state.dragging) return ["Copy", "Save as snippet", "Select All", "Done"];
  return [];
}

/** Timers (injectable for tests). */
export interface Timers {
  set(cb: () => void, ms: number): unknown;
  clear(id: unknown): void;
}

const browserTimers: Timers = {
  set: (cb, ms) => setTimeout(cb, ms),
  clear: (id) => clearTimeout(id as ReturnType<typeof setTimeout>),
};

/**
 * A long press and a tap on the terminal. A finger held within `SLOP_PX` for
 * `LONG_PRESS_MS` is a press (the same hold after which touchScroll leaves a
 * drag alone); lifted sooner without moving, a tap. A finger that moves first
 * is a scroll, neither.
 */
export class LongPress {
  private timer: unknown = null;
  private x0 = 0;
  private y0 = 0;
  /** Down and not yet a scroll. */
  private live = false;
  private fired = false;

  constructor(
    private readonly onPress: (x: number, y: number) => void,
    /** A tap; returns true when it only closed something (the tap then does nothing else, no keyboard). */
    private readonly onTap: () => boolean,
    private readonly timers: Timers = browserTimers,
  ) {}

  start(x: number, y: number) {
    this.clearTimer();
    this.x0 = x;
    this.y0 = y;
    this.live = true;
    this.fired = false;
    this.timer = this.timers.set(() => {
      this.timer = null;
      this.fired = true;
      this.onPress(this.x0, this.y0);
    }, LONG_PRESS_MS);
  }

  /** Returns true when the move belongs to a press that fired (the caller prevents the default). */
  move(x: number, y: number): boolean {
    if (this.fired) return true;
    if (this.live && Math.hypot(x - this.x0, y - this.y0) >= SLOP_PX) {
      this.live = false;
      this.clearTimer();
    }
    return false;
  }

  /** Returns true when the touch must not go on to a click (it was a press, or a tap that closed the menu). */
  end(): boolean {
    this.clearTimer();
    const fired = this.fired;
    const tap = this.live && !fired;
    this.live = false;
    this.fired = false;
    if (fired) return true;
    return tap ? this.onTap() : false;
  }

  cancel() {
    this.clearTimer();
    this.live = false;
    this.fired = false;
  }

  private clearTimer() {
    if (this.timer !== null) this.timers.clear(this.timer);
    this.timer = null;
  }
}

/** Wires `LongPress` to an element's touches (one finger on it). Returns the cleanup. */
export function attachLongPress(el: HTMLElement, lp: LongPress): () => void {
  const onStart = (e: TouchEvent) => {
    if (e.targetTouches.length !== 1) {
      lp.cancel();
      return;
    }
    const t = e.targetTouches[0];
    lp.start(t.clientX, t.clientY);
  };
  const onMove = (e: TouchEvent) => {
    if (e.targetTouches.length !== 1) {
      lp.cancel();
      return;
    }
    const t = e.targetTouches[0];
    if (lp.move(t.clientX, t.clientY) && e.cancelable) e.preventDefault();
  };
  const onEnd = (e: TouchEvent) => {
    if (e.targetTouches.length > 0) return;
    if (lp.end() && e.cancelable) e.preventDefault();
  };
  const onCancel = () => lp.cancel();
  el.addEventListener("touchstart", onStart, { passive: true });
  el.addEventListener("touchmove", onMove, { passive: false });
  el.addEventListener("touchend", onEnd, { passive: false });
  el.addEventListener("touchcancel", onCancel);
  return () => {
    lp.cancel();
    el.removeEventListener("touchstart", onStart);
    el.removeEventListener("touchmove", onMove);
    el.removeEventListener("touchend", onEnd);
    el.removeEventListener("touchcancel", onCancel);
  };
}

/** The terminal's drawn screen as a grid, or null while it has no size. */
export function gridOf(term: Terminal): Grid | null {
  const el = term.element?.querySelector<HTMLElement>(".xterm-screen");
  if (!el) return null;
  const r = el.getBoundingClientRect();
  if (!(r.width > 0 && r.height > 0) || term.cols < 1 || term.rows < 1) return null;
  return {
    left: r.left,
    top: r.top,
    width: r.width,
    height: r.height,
    cols: term.cols,
    rows: term.rows,
    viewportY: term.buffer.active.viewportY,
  };
}

/** A buffer row's cells (see `RowCells`). */
export function rowCells(term: Terminal, row: number): RowCells {
  const line = term.buffer.active.getLine(row);
  const out: RowCells = [];
  for (let x = 0; x < term.cols; x++) {
    const cell = line?.getCell(x);
    if (!cell) out.push(" ");
    else if (cell.getWidth() === 0) out.push(null);
    else out.push(cell.getChars() || " ");
  }
  return out;
}

/** The terminal's selection as a range, or null without one. */
export function selectionOf(term: Terminal): Range | null {
  const p = term.getSelectionPosition();
  if (!p) return null;
  const r = { start: { col: p.start.x, row: p.start.y }, end: { col: p.end.x, row: p.end.y } };
  return indexOf(r.end, term.cols) > indexOf(r.start, term.cols) ? r : null;
}

/** Selects a range in the terminal. */
export function selectRange(term: Terminal, r: Range) {
  term.select(r.start.col, r.start.row, indexOf(r.end, term.cols) - indexOf(r.start, term.cols));
}

/** Selects the word at a cell. */
export function selectWord(term: Terminal, at: Cell) {
  const w = wordRange(rowCells(term, at.row), at.col);
  selectRange(term, { start: { col: w.start, row: at.row }, end: { col: w.end, row: at.row } });
}
