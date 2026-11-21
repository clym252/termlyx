/**
 * Measures main-thread stalls. Uses the Long Tasks API where the webview has
 * it, and always runs a requestAnimationFrame-gap probe as the fallback
 * (WKWebView may not implement 'longtask').
 */
export interface StallSnapshot {
  /** Longest long task (ms); 0 if none or unsupported. */
  maxLongTaskMs: number;
  longTaskSupported: boolean;
  /** Longest gap between animation frames (ms). Includes the ~16 ms frame period. */
  maxRafGapMs: number;
  /** Headline number: longest long task if supported, else the rAF gap. */
  maxStallMs: number;
  source: "longtask" | "raf-gap";
}

export class StallMonitor {
  private observer: PerformanceObserver | null = null;
  private raf = 0;
  private lastFrame = 0;
  private maxLongTask = 0;
  private maxGap = 0;
  private supported = false;
  private running = false;

  start(): void {
    if (this.running) return;
    this.running = true;
    try {
      // Unsupported entry types are silently ignored by observe() in some
      // engines, so check explicitly instead of relying on a throw.
      if (!PerformanceObserver.supportedEntryTypes?.includes("longtask")) {
        throw new Error("longtask unsupported");
      }
      this.observer = new PerformanceObserver((list) => {
        for (const e of list.getEntries()) {
          if (e.duration > this.maxLongTask) this.maxLongTask = e.duration;
        }
      });
      this.observer.observe({ entryTypes: ["longtask"] });
      this.supported = true;
    } catch {
      this.observer = null;
      this.supported = false;
    }
    this.lastFrame = performance.now();
    const tick = (now: number) => {
      const gap = now - this.lastFrame;
      if (gap > this.maxGap) this.maxGap = gap;
      this.lastFrame = now;
      this.raf = requestAnimationFrame(tick);
    };
    this.raf = requestAnimationFrame(tick);
  }

  /** Forget everything seen so far (the probe keeps running). */
  reset(): void {
    this.observer?.takeRecords();
    this.maxLongTask = 0;
    this.maxGap = 0;
    this.lastFrame = performance.now();
  }

  snapshot(): StallSnapshot {
    this.observer?.takeRecords().forEach((e) => {
      if (e.duration > this.maxLongTask) this.maxLongTask = e.duration;
    });
    return {
      maxLongTaskMs: this.maxLongTask,
      longTaskSupported: this.supported,
      maxRafGapMs: this.maxGap,
      maxStallMs: this.supported ? this.maxLongTask : this.maxGap,
      source: this.supported ? "longtask" : "raf-gap",
    };
  }

  stop(): void {
    this.running = false;
    this.observer?.disconnect();
    this.observer = null;
    cancelAnimationFrame(this.raf);
  }
}
