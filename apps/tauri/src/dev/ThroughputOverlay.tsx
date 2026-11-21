import { useEffect, useRef, useState } from "react";
import { RollingRate } from "./benchMetrics";
import { onTerminalWrite } from "./outputTap";
import { StallMonitor } from "./stallMonitor";

/** Cmd/Ctrl+Alt+Shift+P. (The D chords are workspace splits; see workspace/shortcuts.ts.) */
export function isOverlayHotkey(e: KeyboardEvent): boolean {
  return (e.metaKey || e.ctrlKey) && e.altKey && e.shiftKey && e.code === "KeyP";
}

/**
 * Dev overlay: terminal output rate (rolling 1 s, MB/s as written to xterm)
 * and the longest main-thread stall. Renders nothing and subscribes to
 * nothing until toggled on with the hotkey.
 */
export function ThroughputOverlay() {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!isOverlayHotkey(e)) return;
      e.preventDefault();
      e.stopPropagation(); // keep the chord away from the terminal
      setVisible((v) => !v);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  return visible ? <OverlayBody /> : null;
}

function OverlayBody() {
  const rate = useRef(new RollingRate(1000));
  const stalls = useRef(new StallMonitor());
  const [view, setView] = useState({ mbps: 0, stallMs: 0, source: "" });

  useEffect(() => {
    const monitor = stalls.current;
    monitor.start();
    const off = onTerminalWrite((chunk, at) => rate.current.add(chunk.length, at));
    const timer = setInterval(() => {
      const s = monitor.snapshot();
      setView({
        mbps: rate.current.rate(performance.now()),
        stallMs: s.maxStallMs,
        source: s.source,
      });
    }, 250);
    return () => {
      clearInterval(timer);
      off();
      monitor.stop();
    };
  }, []);

  return (
    <div
      onClick={() => stalls.current.reset()}
      title="Click to reset the stall max"
      style={{
        position: "fixed",
        top: 8,
        right: 8,
        zIndex: 1000,
        padding: "6px 10px",
        background: "rgba(0,0,0,0.8)",
        color: "#9f9",
        font: "12px Menlo, 'Cascadia Mono', Consolas, monospace",
        borderRadius: 4,
        pointerEvents: "auto",
      }}
    >
      <div>{view.mbps.toFixed(1)} MB/s</div>
      <div>
        stall {view.stallMs.toFixed(0)} ms ({view.source || "..."})
      </div>
    </div>
  );
}
