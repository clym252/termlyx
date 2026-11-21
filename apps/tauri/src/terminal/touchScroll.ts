import type { IDisposable, Terminal } from "@xterm/xterm";

/**
 * One-finger touch scrolling for the terminal (iOS).
 *
 * xterm 6 scrolls with the mouse wheel only: its viewport is a custom
 * scrollable element (`overflow: hidden`, wheel listeners) with no touch
 * handling, so in a WebView a finger drag on the terminal moved nothing and
 * WebKit bounced the page instead. This turns a vertical drag into scrolling,
 * with momentum after a flick:
 *
 * - normal screen: the scrollback moves with the finger pixel by pixel. xterm
 *   only draws whole rows, so the view is the nearest whole line
 *   (`term.scrollLines`) plus a sub-line shift of the drawn screen (a CSS
 *   transform, under half a line either way). A fling decelerates onto a whole
 *   line and anything else eases back to one: at rest nothing is shifted.
 *   Dragging past either end of the scrollback stretches a little and springs
 *   back (a fling just stops there);
 * - alternate screen (vim, less, htop): whole lines only, and the app gets
 *   wheel input, which xterm sends as mouse wheel reports when the app asked
 *   for mouse reporting and as Up/Down arrow keys otherwise; one per line of
 *   finger travel. A fling there carries on only with mouse reporting, and for
 *   at most `ALT_FLING_MAX_LINES` steps: arrow keys after the finger lifts
 *   would replay shell history (tmux) or move the cursor (vim).
 *
 * A tap, a sideways swipe (the drawer's edge swipe) and a long press (held
 * still before moving) are left alone.
 */

/** Finger travel (CSS px) before a touch counts as a drag rather than a tap. */
export const SLOP_PX = 10;
/** A touch held this long (ms) before it moves is a long press (the copy/paste menu, touchSelect.ts), not a scroll. */
export const LONG_PRESS_MS = 500;
/** Release speed (px/ms) from which a drag carries on as a fling. */
export const MIN_FLING_PX_PER_MS = 0.3;
/** A whole-line fling (alternate screen) ends below this speed (px/ms). */
const STOP_PX_PER_MS = 0.02;
/** A fling keeps this share of its speed each ms (iOS scroll views' normal deceleration rate). */
export const DECELERATION_PER_MS = 0.998;
/** The fling's speed decays by 1/e every this many ms (about 500). */
const DECAY_MS = -1 / Math.log(DECELERATION_PER_MS);
/** The release speed is fitted to the finger's positions over this last part of the drag (ms). */
export const VELOCITY_WINDOW_MS = 80;
/** A finger held still this long (ms) before lifting does not fling. */
export const STILL_MS = 50;
/** A fling never goes faster than this (px/ms). */
const MAX_FLING_PX_PER_MS = 6;
/** One fling frame moves at most this long's worth (ms): a frame late by seconds (the app was in the background) does not land the rest of the fling at once. */
const MAX_FRAME_MS = 50;
/** On the alternate screen (with mouse reporting) a fling sends at most this many wheel steps to the app. */
export const ALT_FLING_MAX_LINES = 25;
/** The ease back onto a whole line (or out of an overscroll) after the finger lifts (ms). */
export const SETTLE_MS = 180;
/** Past an end of the scrollback the view stretches towards (never reaching) this many lines. */
export const OVERSCROLL_LINES = 3;
/** A fling landing on a whole line stops when less than this is left to go (px; under a device pixel at 3x). */
const LAND_PX = 0.25;

/** Where the scrolling goes. */
export interface ScrollSink {
  /**
   * Moves the view `n` whole lines: negative back (towards older output, a
   * finger moving down), positive forward. Returns false when it could not
   * move at all (the end of the scrollback), which ends a fling.
   */
  lines(n: number): boolean;
  /** One line's height in CSS px. */
  lineHeight(): number;
  /** How many lines a fling may move after the finger lifts (0: none); unlimited when absent. */
  flingLimit?(): number;
  /**
   * Pixel-smooth scrolling where the view has it now (the normal screen's
   * scrollback): the whole lines it can still move back (older) and forward.
   * Absent, or null (the alternate screen): whole lines only.
   */
  room?(): { back: number; forward: number } | null;
  /**
   * Draws the view `px` CSS px below its whole-line position (positive: older
   * lines slide in from the top). null: at rest, nothing shifted. Called only
   * while `room()` gives room.
   */
  offset?(px: number | null): void;
  /** Releases what the sink holds (listeners). */
  dispose?(): void;
}

/** Animation frames and a clock (injectable for tests). */
export interface Frames {
  now(): number;
  request(cb: (t: number) => void): number;
  cancel(id: number): void;
}

const browserFrames: Frames = {
  now: () => performance.now(),
  request: (cb) => requestAnimationFrame(cb),
  cancel: (id) => cancelAnimationFrame(id),
};

type Phase = "idle" | "pending" | "scrolling" | "ignored";

/** What runs after the finger lifts. */
type Motion =
  | { kind: "fling"; id: number; v: number; t: number; left: number; land: boolean }
  | { kind: "settle"; id: number };

/** Rubber band past an end: `x` px of over-travel shows as this many px (at most `OVERSCROLL_LINES` lines). */
function stretch(x: number, h: number): number {
  const d = OVERSCROLL_LINES * h;
  return Math.sign(x) * d * (1 - 1 / ((Math.abs(x) * 0.55) / d + 1));
}

/** The over-travel that `stretch` shows as `y` px. */
function unstretch(y: number, h: number): number {
  const d = OVERSCROLL_LINES * h;
  const a = Math.min(Math.abs(y), d * 0.99);
  return (Math.sign(y) * (d / 0.55) * a) / (d - a);
}

/**
 * The finger's speed (px/ms) at time `t`: a least-squares line through its
 * positions over the last `VELOCITY_WINDOW_MS` of movement (one late or
 * bunched touch event barely moves it), 0 when it was held still before `t`.
 */
export function releaseSpeed(samples: readonly { y: number; t: number }[], t: number): number {
  const last = samples[samples.length - 1];
  if (!last || t - last.t > STILL_MS) return 0;
  const recent = samples.filter((s) => last.t - s.t <= VELOCITY_WINDOW_MS);
  if (recent.length < 2) return 0;
  const mt = recent.reduce((a, s) => a + s.t, 0) / recent.length;
  const my = recent.reduce((a, s) => a + s.y, 0) / recent.length;
  let num = 0;
  let den = 0;
  for (const s of recent) {
    num += (s.t - mt) * (s.y - my);
    den += (s.t - mt) ** 2;
  }
  return den > 0 ? num / den : 0;
}

/** The gesture logic, free of the DOM: feed it touch points, it drives the sink. */
export class TouchScroll {
  private phase: Phase = "idle";
  private x0 = 0;
  private y0 = 0;
  private t0 = 0;
  private lastY = 0;
  /** One line's height (px), measured when the touch starts. */
  private h = 0;
  /**
   * Whole lines: finger travel not yet turned into lines. Pixel-smooth: how far
   * (px) the view sits below its whole line, over-travel past an end included.
   */
  private carry = 0;
  /** The shift last drawn (px); null at rest. */
  private shown: number | null = null;
  /** Recent finger positions, for the release speed. */
  private samples: { y: number; t: number }[] = [];
  private motion: Motion | null = null;
  /** This touch caught a running fling: it is the scroller's, whatever it does next. */
  private caught = false;

  constructor(
    private readonly sink: ScrollSink,
    private readonly frames: Frames = browserFrames,
  ) {}

  /** A finger went down. Returns true when the touch is claimed (it stopped a fling): no tap may follow. */
  start(x: number, y: number, t: number): boolean {
    this.caught = this.stop() === "fling";
    this.phase = "pending";
    this.x0 = x;
    this.y0 = y;
    this.t0 = t;
    this.lastY = y;
    this.h = this.sink.lineHeight();
    // Caught mid-motion: the view carries on from where it is drawn.
    this.carry = this.shown === null ? 0 : this.raw(this.shown);
    this.samples = [{ y, t }];
    return this.caught;
  }

  /** The finger moved. Returns true when the move is the scroller's (the caller prevents the default). */
  move(x: number, y: number, t: number): boolean {
    if (this.phase === "pending") {
      const dx = x - this.x0;
      const dy = y - this.y0;
      if (Math.hypot(dx, dy) < SLOP_PX) return this.caught;
      // Held still first: a long press (selection, the callout). Sideways: not ours (the drawer's swipe).
      if (t - this.t0 >= LONG_PRESS_MS || Math.abs(dx) >= Math.abs(dy)) {
        this.phase = "ignored";
        return this.caught;
      }
      this.phase = "scrolling";
    }
    if (this.phase !== "scrolling") return this.caught;
    this.travel(y - this.lastY, true);
    this.lastY = y;
    this.samples.push({ y, t });
    while (this.samples.length > 2 && t - this.samples[0].t > VELOCITY_WINDOW_MS) this.samples.shift();
    return true;
  }

  /** The finger lifted. Returns true when the touch was the scroller's (no tap may follow). */
  end(t: number): boolean {
    const claimed = this.phase === "scrolling" || this.caught;
    if (this.phase === "scrolling") {
      const v = releaseSpeed(this.samples, t);
      const limit = this.sink.flingLimit?.() ?? Infinity;
      if (Math.abs(v) >= MIN_FLING_PX_PER_MS && limit > 0 && !this.overscrolled()) {
        this.startFling(Math.sign(v) * Math.min(Math.abs(v), MAX_FLING_PX_PER_MS), t, limit);
      }
    }
    if (!this.motion) this.settle();
    this.phase = "idle";
    this.caught = false;
    return claimed;
  }

  /** Another finger, or the system took the touch: drop the gesture and any fling, easing back onto a line. */
  cancel() {
    this.stop();
    this.phase = "idle";
    this.caught = false;
    this.settle();
  }

  /** Stops everything at once, nothing left shifted (the terminal is going away). */
  dispose() {
    this.stop();
    this.phase = "idle";
    this.rest();
  }

  /** The pixel-smooth view's room, or null for whole lines only. */
  private room(): { back: number; forward: number } | null {
    return this.sink.offset ? (this.sink.room?.() ?? null) : null;
  }

  /** The view is pulled past an end of the scrollback. */
  private overscrolled(): boolean {
    const room = this.room();
    return !!room && ((this.carry > 0 && room.back <= 0) || (this.carry < 0 && room.forward <= 0));
  }

  /** The carry that draws as `shown` (inverts the stretch past an end). */
  private raw(shown: number): number {
    const room = this.room();
    if (!room || !(this.h > 0)) return 0;
    const over = (shown > 0 && room.back <= 0) || (shown < 0 && room.forward <= 0);
    return over ? unstretch(shown, this.h) : shown;
  }

  private show(px: number | null) {
    if (px === this.shown) return;
    this.shown = px;
    this.sink.offset?.(px);
  }

  /** At rest on a whole line. */
  private rest() {
    this.carry = 0;
    this.show(null);
  }

  /**
   * Finger (`drag`) or fling travel in px, down positive. Returns the whole
   * lines moved (at most `max`), or -1 at the end of the scrollback.
   */
  private travel(dy: number, drag: boolean, max = Infinity): number {
    const h = this.h;
    if (!(h > 0)) return -1;
    const room = this.room();
    if (!room) {
      // Whole lines (or the screen changed under a pixel-smooth gesture).
      if (this.shown !== null) this.rest();
      return this.travelLines(dy, h, max);
    }
    this.carry += dy;
    // The nearest whole line, within the scrollback (positive: back).
    const n = Math.max(-room.forward, Math.min(room.back, Math.round(this.carry / h)));
    // A finger moving down brings older lines into view: back.
    if (n !== 0) this.sink.lines(-n);
    this.carry -= n * h;
    const over = (this.carry > 0 && room.back - n <= 0) || (this.carry < 0 && room.forward + n <= 0);
    if (over && !drag) {
      // A fling stops at the end.
      this.carry = 0;
      this.show(0);
      return -1;
    }
    this.show(over ? stretch(this.carry, h) : this.carry);
    return Math.abs(n);
  }

  /** Whole lines only: lines of travel go to the sink, the rest is carried. */
  private travelLines(dy: number, h: number, max: number): number {
    this.carry += dy;
    let lines = Math.trunc(this.carry / h);
    if (lines === 0) return 0;
    this.carry -= lines * h;
    if (Math.abs(lines) > max) lines = Math.sign(lines) * max;
    return this.sink.lines(-lines) ? Math.abs(lines) : -1;
  }

  private startFling(v: number, t: number, limit: number) {
    // Pixel-smooth: aim the decay to stop on a whole line, in the direction of travel (a tiny change of speed).
    const land = this.room() !== null && this.h > 0;
    if (land) {
      const to = this.carry + v * DECAY_MS;
      const lines = v > 0 ? Math.ceil(to / this.h) : Math.floor(to / this.h);
      v = (lines * this.h - this.carry) / DECAY_MS;
    }
    const step = (now: number) => {
      const f = this.motion;
      if (!f || f.kind !== "fling") return;
      const dt = Math.min(Math.max(0, now - f.t), MAX_FRAME_MS);
      const v1 = f.v * Math.exp(-dt / DECAY_MS);
      // Distance covered over dt as the speed decays from v to v1.
      let d = (f.v - v1) * DECAY_MS;
      // What is still to go is v1 * DECAY_MS: under half a pixel, the landing line is reached.
      const last = f.land ? Math.abs(v1 * DECAY_MS) < LAND_PX : Math.abs(v1) < STOP_PX_PER_MS;
      if (last && f.land) d += v1 * DECAY_MS;
      const sent = this.travel(d, false, f.left);
      if (sent > 0) f.left -= sent;
      if (sent < 0 || f.left <= 0 || last) {
        this.motion = null;
        this.rest();
        return;
      }
      f.v = v1;
      f.t = now;
      f.id = this.frames.request(step);
    };
    this.motion = { kind: "fling", id: this.frames.request(step), v, t, left: limit, land };
  }

  /** Eases a shifted view back onto its whole line (or out of an overscroll). */
  private settle() {
    if (this.shown === null) return;
    if (this.shown === 0) {
      this.rest();
      return;
    }
    const from = this.shown;
    const t0 = this.frames.now();
    const step = (now: number) => {
      const m = this.motion;
      if (!m || m.kind !== "settle") return;
      const k = Math.min(1, Math.max(0, now - t0) / SETTLE_MS);
      if (k >= 1) {
        this.motion = null;
        this.rest();
        return;
      }
      // Ease out (cubic).
      this.show(from * (1 - k) ** 3);
      m.id = this.frames.request(step);
    };
    this.motion = { kind: "settle", id: this.frames.request(step) };
  }

  /** Stops a running fling or settle where it is drawn; returns which one was running. */
  private stop(): Motion["kind"] | null {
    const m = this.motion;
    if (!m) return null;
    this.frames.cancel(m.id);
    this.motion = null;
    return m.kind;
  }
}

/**
 * The terminal as a scroll sink: the scrollback on the normal screen (whole
 * lines through `scrollLines`, the part of a line as a transform of the drawn
 * screen), wheel input to the app on the alternate one.
 */
export function terminalSink(term: Terminal): ScrollSink {
  let h = 0;
  /** The shift asked for (px), from the whole line the buffer is at now; null at rest. */
  let shift: number | null = null;
  /**
   * The buffer line at the top of the drawn rows. xterm draws on its next
   * animation frame, so right after `scrollLines` the rows on screen are
   * still the old ones: the transform makes up the difference until they are
   * redrawn, so the text never jumps a line for a frame.
   */
  let drawnY = term.buffer?.active?.viewportY ?? 0;
  const screen = () => term.element?.querySelector<HTMLElement>(".xterm-screen") ?? null;
  const measure = () => {
    const el = screen();
    const px = el ? el.getBoundingClientRect().height / term.rows : 0;
    h = px > 0 ? px : 17;
    return h;
  };
  const apply = () => {
    const el = screen();
    if (!el) return;
    if (shift === null) {
      el.style.transform = "";
      return;
    }
    const y = shift + (drawnY - term.buffer.active.viewportY) * (h || measure());
    el.style.transform = `translate3d(0, ${y}px, 0)`;
  };
  const rendered: IDisposable | undefined = term.onRender?.(() => {
    drawnY = term.buffer.active.viewportY;
    if (shift !== null) apply();
  });
  return {
    lineHeight: measure,
    flingLimit() {
      if (term.buffer.active.type !== "alternate") return Infinity;
      // Without mouse reporting each step is an arrow key: a fling would replay shell history
      // (tmux) or run the cursor through a file (vim). The drag alone tracks the finger.
      return term.modes.mouseTrackingMode === "none" ? 0 : ALT_FLING_MAX_LINES;
    },
    room() {
      const buf = term.buffer.active;
      if (buf.type === "alternate") return null;
      return { back: buf.viewportY, forward: buf.baseY - buf.viewportY };
    },
    offset(px) {
      shift = px;
      apply();
    },
    dispose() {
      rendered?.dispose();
      shift = null;
      apply();
    },
    lines(n) {
      const buf = term.buffer.active;
      if (buf.type === "alternate") {
        const el = term.element;
        if (!el) return false;
        const box = el.getBoundingClientRect();
        // xterm turns each wheel event into one wheel report (mouse reporting on) or one arrow key (off).
        for (let i = 0; i < Math.abs(n); i++) {
          el.dispatchEvent(
            new WheelEvent("wheel", {
              deltaY: Math.sign(n),
              deltaMode: WheelEvent.DOM_DELTA_LINE,
              clientX: box.left + box.width / 2,
              clientY: box.top + box.height / 2,
              bubbles: true,
              cancelable: true,
            }),
          );
        }
        return true;
      }
      const before = buf.viewportY;
      if ((n < 0 && before <= 0) || (n > 0 && before >= buf.baseY)) return false;
      term.scrollLines(n);
      return true;
    },
  };
}

/**
 * Wires `TouchScroll` to an element's touch events (non-passive, so a scroll
 * can cancel the page's own panning and the click after it). Returns the
 * cleanup, which also disposes the sink.
 */
export function attachTouchScroll(el: HTMLElement, sink: ScrollSink, frames: Frames = browserFrames): () => void {
  const ts = new TouchScroll(sink, frames);
  // Only fingers on the terminal count: a thumb holding a key bar arrow is not a second finger here.
  const onStart = (e: TouchEvent) => {
    if (e.targetTouches.length !== 1) {
      ts.cancel();
      return;
    }
    const t = e.targetTouches[0];
    if (ts.start(t.clientX, t.clientY, frames.now()) && e.cancelable) e.preventDefault();
  };
  const onMove = (e: TouchEvent) => {
    if (e.targetTouches.length !== 1) {
      ts.cancel();
      return;
    }
    const t = e.targetTouches[0];
    if (ts.move(t.clientX, t.clientY, frames.now()) && e.cancelable) e.preventDefault();
  };
  const onEnd = (e: TouchEvent) => {
    if (e.targetTouches.length > 0) return;
    if (ts.end(frames.now()) && e.cancelable) e.preventDefault();
  };
  const onCancel = () => ts.cancel();
  el.addEventListener("touchstart", onStart, { passive: false });
  el.addEventListener("touchmove", onMove, { passive: false });
  el.addEventListener("touchend", onEnd, { passive: false });
  el.addEventListener("touchcancel", onCancel);
  return () => {
    ts.dispose();
    sink.dispose?.();
    el.removeEventListener("touchstart", onStart);
    el.removeEventListener("touchmove", onMove);
    el.removeEventListener("touchend", onEnd);
    el.removeEventListener("touchcancel", onCancel);
  };
}
