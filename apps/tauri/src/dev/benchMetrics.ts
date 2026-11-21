/** Pure helpers for the throughput spike. No DOM or Tauri imports, so they unit-test cleanly. */

const encoder = new TextEncoder();

/**
 * Finds a fixed marker in a byte stream delivered in arbitrary frames,
 * including a marker split across two (or more) frames. Positions are
 * absolute stream offsets, so callers can count the bytes between markers.
 *
 * The marker should start with a byte that never occurs in the payload being
 * measured (the bench uses '@', which is not in the base64 alphabet), so the
 * scan is a native `indexOf` plus the occasional short compare.
 */
export class MarkerScanner {
  private readonly marker: Uint8Array;
  private tail = new Uint8Array(0);
  private consumed = 0;

  constructor(marker: string) {
    this.marker = encoder.encode(marker);
    if (this.marker.length === 0) throw new Error("empty marker");
  }

  /** Feed the next frame; returns the marker's absolute [start, end) offsets the first time it completes. */
  push(chunk: Uint8Array): { start: number; end: number } | null {
    const m = this.marker;
    const tailLen = this.tail.length;
    const hay = tailLen === 0 ? chunk : concat(this.tail, chunk);
    const base = this.consumed - tailLen; // absolute offset of hay[0]
    this.consumed += chunk.length;

    const first = m[0]!;
    let i = hay.indexOf(first);
    while (i !== -1 && i + m.length <= hay.length) {
      let j = 1;
      while (j < m.length && hay[i + j] === m[j]) j++;
      if (j === m.length) {
        this.tail = new Uint8Array(0);
        return { start: base + i, end: base + i + m.length };
      }
      i = hay.indexOf(first, i + 1);
    }
    // Keep only what could still be the prefix of a marker.
    const keep = Math.min(m.length - 1, hay.length);
    this.tail = hay.slice(hay.length - keep);
    return null;
  }
}

function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}

/** Megabytes (1e6 bytes) per second; 0 when no time has elapsed. */
export function mbPerSec(bytes: number, ms: number): number {
  if (ms <= 0) return 0;
  return bytes / 1e6 / (ms / 1000);
}

/** Rolling-window byte counter (default 1 s) for the live overlay. */
export class RollingRate {
  private samples: Array<{ t: number; bytes: number }> = [];
  constructor(private readonly windowMs = 1000) {}

  add(bytes: number, now: number): void {
    this.samples.push({ t: now, bytes });
    this.trim(now);
  }

  /** MB/s over the last window ending at `now`. */
  rate(now: number): number {
    this.trim(now);
    let total = 0;
    for (const s of this.samples) total += s.bytes;
    return mbPerSec(total, this.windowMs);
  }

  private trim(now: number): void {
    const cutoff = now - this.windowMs;
    let n = 0;
    while (n < this.samples.length && this.samples[n]!.t <= cutoff) n++;
    if (n > 0) this.samples.splice(0, n);
  }
}

/** Gate from the task: at least 30 MB/s and no main-thread stall over 250 ms. */
export const GATE_MIN_MBPS = 30;
export const GATE_MAX_STALL_MS = 250;

export function passesGate(mbps: number, maxStallMs: number): boolean {
  return mbps >= GATE_MIN_MBPS && maxStallMs <= GATE_MAX_STALL_MS;
}
