import { describe, expect, it } from "vitest";
import {
  MarkerScanner,
  RollingRate,
  mbPerSec,
  passesGate,
} from "./benchMetrics";

const bytes = (s: string) => new TextEncoder().encode(s);

describe("MarkerScanner", () => {
  it("finds a marker inside one frame with absolute offsets", () => {
    const s = new MarkerScanner("@@START@@");
    expect(s.push(bytes("hello @@START@@ world"))).toEqual({ start: 6, end: 15 });
  });

  it("finds a marker split across two frames", () => {
    const s = new MarkerScanner("@@START@@");
    expect(s.push(bytes("0123456789@@STA"))).toBeNull();
    // marker began at offset 10, finishes 6 bytes into the second frame
    expect(s.push(bytes("RT@@abc"))).toEqual({ start: 10, end: 19 });
  });

  it("finds a marker split one byte at a time", () => {
    const s = new MarkerScanner("@@END@@");
    const all = bytes("xx@@END@@yy");
    let hit: { start: number; end: number } | null = null;
    for (const b of all) hit = s.push(Uint8Array.of(b)) ?? hit;
    expect(hit).toEqual({ start: 2, end: 9 });
  });

  it("ignores a lookalike prefix and false starts", () => {
    const s = new MarkerScanner("@@START@@");
    expect(s.push(bytes("@@STAR @@ST"))).toBeNull();
    expect(s.push(bytes("ARX@@"))).toBeNull(); // "@@ST" + "ARX@@" is not the marker
    expect(s.push(bytes("..@@START@@"))).toEqual({ start: 18, end: 27 });
  });

  it("does not match the echoed command line (format specifiers, not the marker)", () => {
    const s = new MarkerScanner("@@TETHER_BENCH_START@@");
    expect(s.push(bytes("printf '@@TETHER_%s_%s@@\\n' BENCH START"))).toBeNull();
  });

  it("counts the bytes between two markers across frames", () => {
    const start = new MarkerScanner("@@S@@");
    const end = new MarkerScanner("@@E@@");
    const frames = [bytes("junk@@S"), bytes("@@PAYLOAD-"), bytes("PAYLOAD@"), bytes("@E@@tail")];
    let s: { start: number; end: number } | null = null;
    let e: { start: number; end: number } | null = null;
    for (const f of frames) {
      s = s ?? start.push(f);
      e = e ?? end.push(f);
    }
    expect(s).not.toBeNull();
    expect(e).not.toBeNull();
    expect(e!.start - s!.end).toBe("PAYLOAD-PAYLOAD".length);
  });
});

describe("mbPerSec", () => {
  it("computes megabytes per second", () => {
    expect(mbPerSec(100_000_000, 2000)).toBeCloseTo(50);
    expect(mbPerSec(30_000_000, 1000)).toBeCloseTo(30);
  });
  it("is 0 for no elapsed time", () => {
    expect(mbPerSec(5, 0)).toBe(0);
  });
});

describe("RollingRate", () => {
  it("reports MB/s over the last second and drops old samples", () => {
    const r = new RollingRate(1000);
    r.add(10_000_000, 100);
    r.add(20_000_000, 600);
    expect(r.rate(900)).toBeCloseTo(30);
    expect(r.rate(1200)).toBeCloseTo(20); // first sample aged out
    expect(r.rate(2000)).toBe(0);
  });
});

describe("passesGate", () => {
  it("needs >=30 MB/s and no stall over 250 ms", () => {
    expect(passesGate(30, 250)).toBe(true);
    expect(passesGate(29.9, 10)).toBe(false);
    expect(passesGate(80, 251)).toBe(false);
  });
});
