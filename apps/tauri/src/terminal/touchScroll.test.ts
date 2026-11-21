import { Terminal } from "@xterm/xterm";
import { describe, expect, it, vi } from "vitest";
import {
  ALT_FLING_MAX_LINES,
  DECELERATION_PER_MS,
  LONG_PRESS_MS,
  OVERSCROLL_LINES,
  SETTLE_MS,
  SLOP_PX,
  STILL_MS,
  TouchScroll,
  attachTouchScroll,
  releaseSpeed,
  terminalSink,
  type Frames,
  type ScrollSink,
} from "./touchScroll";

const LINE = 20;

/** A sink that records the line steps it is given; `room` limits how far it can go each way. */
function sink(room = { back: Infinity, forward: Infinity }, flingLimit?: number) {
  const steps: number[] = [];
  let pos = 0;
  const s: ScrollSink & { steps: number[]; total(): number } = {
    steps,
    ...(flingLimit === undefined ? {} : { flingLimit: () => flingLimit }),
    total: () => steps.reduce((a, b) => a + b, 0),
    lineHeight: () => LINE,
    lines(n) {
      const next = Math.max(-room.back, Math.min(room.forward, pos + n));
      const moved = next !== pos;
      pos = next;
      steps.push(n);
      return moved;
    },
  };
  return s;
}

/** Animation frames driven by hand: `tick(ms)` advances the clock and runs the frame due. */
function frames() {
  let now = 0;
  let pending: ((t: number) => void) | null = null;
  const f: Frames & { tick(ms: number): void; running(): boolean; flush(): void } = {
    now: () => now,
    request(cb) {
      pending = cb;
      return 1;
    },
    cancel() {
      pending = null;
    },
    tick(ms) {
      now += ms;
      const cb = pending;
      pending = null;
      cb?.(now);
    },
    running: () => pending !== null,
    flush() {
      for (let i = 0; i < 2000 && pending; i++) f.tick(16);
    },
  };
  return f;
}

/** A one-finger drag along y (x fixed), `stepMs` apart; returns whether moves were claimed. */
function drag(ts: TouchScroll, f: ReturnType<typeof frames>, ys: number[], stepMs = 16, x = 100) {
  ts.start(x, ys[0], f.now());
  const claimed: boolean[] = [];
  for (const y of ys.slice(1)) {
    f.tick(stepMs);
    claimed.push(ts.move(x, y, f.now()));
  }
  return claimed;
}

describe("TouchScroll", () => {
  it("a_drag_down_scrolls_back_one_line_per_line_height", () => {
    const s = sink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    // Slow: 10 px every 50 ms, 200 px in all (past the slop, then 10 lines).
    const ys = Array.from({ length: 21 }, (_, i) => 100 + i * 10);
    const claimed = drag(ts, f, ys, 50);
    expect(claimed[claimed.length - 1]).toBe(true);
    // The slop is not lost: the whole 200 px counts.
    expect(s.total()).toBe(-10);
    expect(ts.end(f.now() + 50)).toBe(true);
    // Too slow to fling.
    expect(f.running()).toBe(false);
  });

  it("a_drag_up_scrolls_forward", () => {
    const s = sink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [300, 280, 260, 240, 220, 200], 50);
    expect(s.total()).toBe(5);
  });

  it("small_moves_under_the_slop_are_a_tap_and_left_alone", () => {
    const s = sink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    const claimed = drag(ts, f, [100, 100 + SLOP_PX - 2, 100 + SLOP_PX - 4]);
    expect(claimed).toEqual([false, false]);
    expect(ts.end(f.now())).toBe(false);
    expect(s.steps).toEqual([]);
  });

  it("a_sideways_drag_is_not_a_scroll", () => {
    const s = sink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    ts.start(5, 300, 0);
    f.tick(16);
    expect(ts.move(40, 305, f.now())).toBe(false);
    f.tick(16);
    expect(ts.move(120, 340, f.now())).toBe(false);
    expect(ts.end(f.now())).toBe(false);
    expect(s.steps).toEqual([]);
  });

  it("a_long_press_before_moving_is_left_to_selection", () => {
    const s = sink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    ts.start(100, 100, 0);
    f.tick(LONG_PRESS_MS + 10);
    expect(ts.move(100, 200, f.now())).toBe(false);
    expect(s.steps).toEqual([]);
  });

  it("a_fling_keeps_going_after_the_finger_lifts_then_stops", () => {
    const s = sink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    // 30 px per 16 ms: about 1.9 px/ms.
    drag(ts, f, [100, 130, 160, 190, 220, 250]);
    const atLift = s.total();
    expect(ts.end(f.now())).toBe(true);
    expect(f.running()).toBe(true);
    f.flush();
    expect(f.running()).toBe(false);
    // It went on well past where the finger stopped, in the same direction.
    expect(s.total()).toBeLessThan(atLift - 10);
    expect(s.steps.every((n) => n < 0)).toBe(true);
  });

  it("a_fling_stops_at_the_end_of_the_scrollback", () => {
    const s = sink({ back: 12, forward: 0 });
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 130, 160, 190, 220, 250]);
    ts.end(f.now());
    let ticks = 0;
    while (f.running() && ticks < 2000) {
      f.tick(16);
      ticks++;
    }
    // Stopped soon after it could go no further, not after the whole fling.
    expect(ticks).toBeLessThan(40);
  });

  it("a_touch_during_a_fling_stops_it_and_is_claimed", () => {
    const s = sink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 130, 160, 190, 220, 250]);
    ts.end(f.now());
    f.tick(16);
    expect(f.running()).toBe(true);
    // The catching touch is the scroller's (no tap, no keyboard).
    expect(ts.start(100, 400, f.now())).toBe(true);
    expect(f.running()).toBe(false);
    const steps = s.steps.length;
    f.tick(16);
    expect(s.steps.length).toBe(steps);
    expect(ts.end(f.now())).toBe(true);
    // A plain touch with no fling running is not claimed.
    expect(ts.start(100, 400, f.now())).toBe(false);
  });

  it("a_second_finger_cancels_the_gesture", () => {
    const s = sink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 140, 180], 50);
    const before = s.steps.length;
    ts.cancel();
    expect(ts.move(100, 300, f.now() + 16)).toBe(false);
    expect(ts.end(f.now() + 32)).toBe(false);
    expect(s.steps.length).toBe(before);
    expect(f.running()).toBe(false);
  });

  it("no_fling_where_the_sink_allows_none_only_the_drag", () => {
    // The alternate screen without mouse reporting: a fling would be a burst of arrow keys.
    const s = sink(undefined, 0);
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 130, 160, 190, 220, 250]);
    const byFinger = s.total();
    expect(byFinger).toBe(-7);
    expect(ts.end(f.now())).toBe(true);
    expect(f.running()).toBe(false);
    expect(s.total()).toBe(byFinger);
  });

  it("a_limited_fling_stops_after_its_lines", () => {
    const s = sink(undefined, ALT_FLING_MAX_LINES);
    const f = frames();
    const ts = new TouchScroll(s, f);
    // A hard flick: uncapped it would run well past the limit.
    drag(ts, f, [100, 160, 220, 280, 340, 400]);
    const byFinger = s.total();
    ts.end(f.now());
    f.flush();
    expect(f.running()).toBe(false);
    expect(byFinger - s.total()).toBe(ALT_FLING_MAX_LINES);
  });

  it("a_long_frame_gap_does_not_land_the_whole_fling_at_once", () => {
    const s = sink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 130, 160, 190, 220, 250]);
    ts.end(f.now());
    const before = s.steps.length;
    // The app was in the background for 5 s mid-fling: one frame moves at most 50 ms worth.
    f.tick(5000);
    const jumped = s.steps.slice(before).reduce((a, b) => a + b, 0);
    // About 1.9 px/ms for 50 ms is under 5 lines of 20 px.
    expect(Math.abs(jumped)).toBeLessThanOrEqual(5);
    expect(f.running()).toBe(true);
  });
});

/**
 * A pixel-smooth sink: a scrollback of `lines` lines above the bottom, the view `at` lines from
 * the top (0: oldest). Records every offset drawn; `y()` is the view's position in px from the top.
 */
function smoothSink(lines = 1000, at = lines) {
  const offsets: (number | null)[] = [];
  const s: ScrollSink & { pos: number; offsets: (number | null)[]; shown(): number | null; y(): number } = {
    pos: at,
    offsets,
    shown: () => (offsets.length ? offsets[offsets.length - 1] : null),
    // Offsets are positive down (older lines in view): the view's top is that far above the line.
    y: () => s.pos * LINE - (s.shown() ?? 0),
    lineHeight: () => LINE,
    room: () => ({ back: s.pos, forward: lines - s.pos }),
    offset: (px) => offsets.push(px),
    lines(n) {
      const next = Math.max(0, Math.min(lines, s.pos + n));
      const moved = next !== s.pos;
      s.pos = next;
      return moved;
    },
  };
  return s;
}

describe("TouchScroll pixel-smooth", () => {
  it("a_drag_moves_the_view_by_the_pixel_not_the_line", () => {
    const s = smoothSink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    const y0 = s.y();
    // 12 px (past the slop), then 3 px more, then 4 more: every pixel shows.
    drag(ts, f, [100, 112, 115, 119], 50);
    expect(y0 - s.y()).toBeCloseTo(19);
    // Under half a line either way from the whole line it sits on.
    for (const o of s.offsets) expect(Math.abs(o ?? 0)).toBeLessThanOrEqual(LINE / 2);
  });

  it("whole_lines_commit_as_the_travel_crosses_half_a_line", () => {
    const s = smoothSink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    ts.start(100, 100, 0);
    expect(ts.move(100, 109.9 + SLOP_PX, 16)).toBe(true);
    // 19.9 px down: the nearest whole line is one back, drawn 0.1 px above it.
    expect(s.pos).toBe(999);
    expect(s.shown()).toBeCloseTo(-0.1);
    ts.move(100, 100 + 29, 32);
    expect(s.pos).toBe(999);
    expect(s.shown()).toBeCloseTo(9);
    ts.move(100, 100 + 31, 48);
    expect(s.pos).toBe(998);
    expect(s.shown()).toBeCloseTo(-9);
  });

  it("a_slow_release_eases_back_onto_the_nearest_line_and_rests", () => {
    const s = smoothSink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 112, 124, 136], 50);
    // 36 px: two lines back, 4 px short of the second.
    expect(s.pos).toBe(998);
    expect(s.shown()).toBeCloseTo(-4);
    f.tick(STILL_MS + 10);
    ts.end(f.now());
    expect(f.running()).toBe(true);
    f.tick(SETTLE_MS / 2);
    // Easing: part way, the same side.
    expect(s.shown()).toBeLessThan(0);
    expect(s.shown()).toBeGreaterThan(-4);
    f.flush();
    expect(f.running()).toBe(false);
    expect(s.shown()).toBeNull();
    expect(s.pos).toBe(998);
  });

  it("a_fling_decelerates_onto_a_whole_line_with_nothing_left_shifted", () => {
    const s = smoothSink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 130, 160, 190, 220, 250]);
    const atLift = s.pos;
    ts.end(f.now());
    let steps = 0;
    let prevY = s.y();
    let prevStep = Infinity;
    while (f.running() && steps < 5000) {
      f.tick(8);
      steps++;
      const y = s.y();
      // Always back (older), never faster than the frame before: a smooth deceleration (the
      // landing frame adds the last fraction of a pixel).
      expect(y).toBeLessThanOrEqual(prevY + 1e-9);
      expect(prevY - y).toBeLessThanOrEqual(f.running() ? prevStep + 1e-6 : prevStep + 0.25);
      prevStep = prevY - y;
      prevY = y;
    }
    expect(f.running()).toBe(false);
    expect(s.shown()).toBeNull();
    // About v / -ln(0.998) px: 30 px per 16 ms is ~940 px, ~47 lines.
    const flung = atLift - s.pos;
    const expected = (30 / 16) * (-1 / Math.log(DECELERATION_PER_MS)) / LINE;
    expect(flung).toBeGreaterThanOrEqual(Math.floor(expected));
    expect(flung).toBeLessThanOrEqual(Math.ceil(expected) + 1);
    // The last drawn shift before rest was under a pixel: no visible snap at the end.
    const shifts = s.offsets.filter((o): o is number => o !== null);
    expect(Math.abs(shifts[shifts.length - 1])).toBeLessThan(1);
  });

  it("decelerates_at_ios_rate", () => {
    const s = smoothSink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 130, 160, 190, 220, 250]);
    ts.end(f.now());
    const speed = () => {
      const a = s.y();
      f.tick(10);
      return (a - s.y()) / 10;
    };
    const v0 = speed();
    for (let i = 0; i < 20; i++) f.tick(10);
    const v1 = speed();
    // 220 ms later: 0.998^220 of the speed (within rounding of the frame sums).
    expect(v1 / v0).toBeCloseTo(DECELERATION_PER_MS ** 220, 1);
  });

  it("stops_at_the_top_of_the_scrollback_with_a_small_stretch_then_springs_back", () => {
    const s = smoothSink(1000, 2);
    const f = frames();
    const ts = new TouchScroll(s, f);
    // 200 px down with only 2 lines (40 px) above: the rest stretches, a little.
    drag(ts, f, Array.from({ length: 21 }, (_, i) => 100 + i * 10), 50);
    expect(s.pos).toBe(0);
    const pulled = s.shown()!;
    expect(pulled).toBeGreaterThan(0);
    expect(pulled).toBeLessThan(OVERSCROLL_LINES * LINE);
    expect(pulled).toBeLessThan(200 - 40);
    // Released (even with speed): no fling past the end; it eases back to rest.
    ts.end(f.now());
    f.flush();
    expect(s.shown()).toBeNull();
    expect(s.pos).toBe(0);
  });

  it("a_fling_into_the_bottom_stops_there_on_a_whole_line", () => {
    const s = smoothSink(1000, 995);
    const f = frames();
    const ts = new TouchScroll(s, f);
    // Finger up the screen, fast: forward, 5 lines from the bottom.
    drag(ts, f, [400, 370, 340, 310, 280, 250]);
    ts.end(f.now());
    let ticks = 0;
    while (f.running() && ticks < 2000) {
      f.tick(16);
      ticks++;
    }
    expect(ticks).toBeLessThan(40);
    expect(s.pos).toBe(1000);
    expect(s.shown()).toBeNull();
  });

  it("a_touch_during_the_ease_back_carries_on_from_where_it_is_drawn", () => {
    const s = smoothSink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 112, 124, 136], 50);
    f.tick(STILL_MS + 10);
    ts.end(f.now());
    f.tick(SETTLE_MS / 3);
    const y = s.y();
    // Not a fling: the touch is not claimed, and nothing jumps.
    expect(ts.start(100, 300, f.now())).toBe(false);
    expect(f.running()).toBe(false);
    expect(s.y()).toBeCloseTo(y);
    f.tick(16);
    ts.move(100, 300 + SLOP_PX + 2, f.now());
    expect(y - s.y()).toBeCloseTo(SLOP_PX + 2);
    // A tap on a shifted view still eases it back.
    f.tick(STILL_MS + 10);
    ts.end(f.now());
    f.flush();
    expect(s.shown()).toBeNull();
  });

  it("a_tap_on_a_view_caught_mid_ease_still_rests_on_a_line", () => {
    const s = smoothSink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 112, 124, 136], 50);
    f.tick(STILL_MS + 10);
    ts.end(f.now());
    f.tick(SETTLE_MS / 3);
    ts.start(100, 300, f.now());
    expect(ts.end(f.now() + 30)).toBe(false);
    f.flush();
    expect(s.shown()).toBeNull();
  });

  it("whole_lines_only_where_the_sink_has_no_room_to_shift", () => {
    // The alternate screen: room() is null, no offsets, the old whole-line steps.
    const s = smoothSink();
    s.room = () => null;
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 112, 124, 136, 148], 50);
    expect(s.offsets).toEqual([]);
    expect(s.pos).toBe(998);
  });

  it("dispose_leaves_nothing_shifted", () => {
    const s = smoothSink();
    const f = frames();
    const ts = new TouchScroll(s, f);
    drag(ts, f, [100, 130, 160, 190, 220, 250]);
    ts.end(f.now());
    f.tick(16);
    ts.dispose();
    expect(f.running()).toBe(false);
    expect(s.shown()).toBeNull();
  });
});

describe("releaseSpeed", () => {
  it("fits_the_last_80_ms_not_the_whole_drag", () => {
    // Slow for 300 ms, then 2 px/ms for the last 80.
    const samples = [
      ...Array.from({ length: 20 }, (_, i) => ({ y: i * 2, t: i * 16 })),
      ...Array.from({ length: 6 }, (_, i) => ({ y: 38 + (i + 1) * 32, t: 304 + (i + 1) * 16 })),
    ];
    expect(releaseSpeed(samples, 400 + 8)).toBeCloseTo(2, 1);
  });

  it("is_steady_under_uneven_event_timing", () => {
    // 1.5 px/ms, events arriving 8, 20, 12, 16 ms apart (positions true to their times).
    const ts = [0, 8, 28, 40, 56, 64, 84];
    const samples = ts.map((t) => ({ t, y: 1.5 * t }));
    expect(releaseSpeed(samples, 90)).toBeCloseTo(1.5, 5);
  });

  it("is_zero_when_the_finger_stopped_before_lifting", () => {
    const samples = [0, 16, 32, 48].map((t) => ({ t, y: 2 * t }));
    expect(releaseSpeed(samples, 48 + STILL_MS + 1)).toBe(0);
    expect(releaseSpeed(samples.slice(0, 1), 10)).toBe(0);
  });
});

describe("terminalSink", () => {
  /** A terminal on the alternate screen with the given mouse mode (only what the sink reads). */
  const altTerm = (mouse: "none" | "vt200") =>
    ({
      buffer: { active: { type: "alternate" } },
      modes: { mouseTrackingMode: mouse },
    }) as unknown as Terminal;

  /** A normal-screen terminal whose rows are redrawn only when `render()` is called (as xterm does, a frame later). */
  function normalTerm(viewportY = 100, baseY = 100) {
    const element = document.createElement("div");
    const screen = document.createElement("div");
    screen.className = "xterm-screen";
    element.append(screen);
    screen.getBoundingClientRect = () => ({ height: 10 * LINE }) as DOMRect;
    const listeners = new Set<() => void>();
    const active = { type: "normal", viewportY, baseY };
    const term = {
      element,
      rows: 10,
      buffer: { active },
      modes: { mouseTrackingMode: "none" },
      scrollLines: (n: number) => {
        active.viewportY = Math.max(0, Math.min(active.baseY, active.viewportY + n));
      },
      onRender: (l: () => void) => {
        listeners.add(l);
        return { dispose: () => listeners.delete(l) };
      },
    };
    return { term: term as unknown as Terminal, screen, active, render: () => listeners.forEach((l) => l()), listeners };
  }

  it("shifts_the_drawn_screen_and_covers_the_frame_before_xterm_redraws", () => {
    const { term, screen, render, active } = normalTerm();
    const s = terminalSink(term);
    expect(s.lineHeight()).toBe(LINE);
    expect(s.room?.()).toEqual({ back: 100, forward: 0 });
    s.offset?.(7);
    expect(screen.style.transform).toBe("translate3d(0, 7px, 0)");
    // One line back, not yet redrawn: the old rows move down a whole line more, so nothing jumps.
    s.lines(-1);
    expect(active.viewportY).toBe(99);
    s.offset?.(-8);
    expect(screen.style.transform).toBe(`translate3d(0, ${LINE - 8}px, 0)`);
    // Redrawn: just the sub-line part.
    render();
    expect(screen.style.transform).toBe("translate3d(0, -8px, 0)");
    // At rest: no transform, and new output at rest is left alone.
    s.offset?.(null);
    expect(screen.style.transform).toBe("");
    active.viewportY = 100;
    expect(screen.style.transform).toBe("");
    render();
    expect(screen.style.transform).toBe("");
  });

  it("dispose_clears_the_shift_and_the_render_listener", () => {
    const { term, screen, listeners } = normalTerm();
    const s = terminalSink(term);
    s.offset?.(5);
    s.dispose?.();
    expect(screen.style.transform).toBe("");
    expect(listeners.size).toBe(0);
  });

  it("has_no_room_to_shift_on_the_alternate_screen", () => {
    expect(terminalSink(altTerm("none")).room?.()).toBeNull();
  });

  it("flings_on_the_alternate_screen_only_with_mouse_reporting_and_then_a_few_lines", () => {
    expect(terminalSink(altTerm("none")).flingLimit?.()).toBe(0);
    expect(terminalSink(altTerm("vt200")).flingLimit?.()).toBe(ALT_FLING_MAX_LINES);
    const normal = { buffer: { active: { type: "normal" } }, modes: { mouseTrackingMode: "none" } } as unknown as Terminal;
    expect(terminalSink(normal).flingLimit?.() ?? Infinity).toBe(Infinity);
  });
});

describe("attachTouchScroll", () => {
  /** A touch event on `el` with these fingers on the element and `others` elsewhere (the key bar). */
  function touch(el: HTMLElement, type: string, onEl: { x: number; y: number }[], others = 0) {
    const mk = (p: { x: number; y: number }, i: number) => ({ identifier: i, clientX: p.x, clientY: p.y, target: el });
    const target = onEl.map(mk);
    const elsewhere = Array.from({ length: others }, (_, i) => ({ identifier: 10 + i, clientX: 0, clientY: 900, target: document.body }));
    const e = new Event(type, { bubbles: true, cancelable: true });
    Object.assign(e, { touches: [...target, ...elsewhere], targetTouches: target, changedTouches: target });
    el.dispatchEvent(e);
  }

  it("a_thumb_resting_on_the_key_bar_does_not_cancel_the_scroll", () => {
    const el = document.createElement("div");
    document.body.append(el);
    const lines = vi.fn(() => true);
    let now = 0;
    const f: Frames = { now: () => now, request: () => 1, cancel: () => {} };
    const off = attachTouchScroll(el, { lineHeight: () => LINE, lines }, f);
    // One finger holds an arrow key on the bar (outside el) while another drags the terminal.
    touch(el, "touchstart", [{ x: 100, y: 100 }], 1);
    for (let y = 120; y <= 200; y += 20) {
      now += 50;
      touch(el, "touchmove", [{ x: 100, y }], 1);
    }
    expect(lines).toHaveBeenCalled();
    expect(lines.mock.calls.reduce((a, c) => a + (c as unknown as [number])[0], 0)).toBe(-5);
    off();
    el.remove();
  });
});
