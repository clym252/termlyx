import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useMemo, useRef, useState } from "react";
import type { BenchConfig } from "../dev/bench";
import { useBench } from "../dev/useBench";
import { tapShellTerm } from "../dev/outputTap";
import { terminalKeyHandler } from "../workspace/clipboardKeys";
import { useShell, type ConnectInput, type ShellTarget } from "./useShell";

export function TerminalView({
  params,
  onExit,
  bench,
}: {
  params: ConnectInput;
  onExit: () => void;
  /** Present only when the app was launched with TETHER_BENCH=1. */
  bench?: BenchConfig;
}) {
  const host = useRef<HTMLDivElement>(null);
  const [term, setTerm] = useState<Terminal | null>(null);
  const renderer = useRef<"webgl" | "dom">("dom");

  useEffect(() => {
    const el = host.current;
    if (!el) return;

    const t = new Terminal({
      cursorBlink: true,
      fontFamily: "Menlo, Monaco, 'Cascadia Mono', Consolas, 'Courier New', monospace",
      fontSize: 13,
    });
    const fit = new FitAddon();
    t.loadAddon(fit);
    t.open(el);
    // The palette and workspace shortcuts are not terminal input; copy/paste keys.
    t.attachCustomKeyEventHandler(terminalKeyHandler(t));

    // GPU renderer when available; the DOM renderer is the fallback on error
    // or if the GL context is later lost.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => {
        renderer.current = "dom";
        webgl.dispose();
      });
      t.loadAddon(webgl);
      renderer.current = "webgl";
    } catch (err) {
      console.warn("WebGL renderer unavailable, using DOM renderer", err);
    }

    fit.fit();
    // fit() changes cols/rows, which fires term.onResize -> resize_shell (see useShell).
    // Debounced so a drag-resize does not flood the session with intermediate sizes.
    let fitTimer: ReturnType<typeof setTimeout> | undefined;
    const scheduleFit = () => {
      clearTimeout(fitTimer);
      fitTimer = setTimeout(() => fit.fit(), 50);
    };
    window.addEventListener("resize", scheduleFit);
    const observer = new ResizeObserver(scheduleFit);
    observer.observe(el);
    setTerm(t);
    t.focus();

    return () => {
      clearTimeout(fitTimer);
      observer.disconnect();
      window.removeEventListener("resize", scheduleFit);
      setTerm(null);
      t.dispose();
    };
  }, []);

  // Dev tooling (throughput overlay, bench) observes output through a thin
  // wrapper; production builds hand xterm to the shell directly.
  const shellTerm = useMemo(
    () => (term ? (import.meta.env.DEV ? tapShellTerm(term) : term) : null),
    [term],
  );
  const target = useMemo<ShellTarget>(() => ({ kind: "direct", params }), [params]);
  const { status, error } = useShell(shellTerm, target);

  // Bench mode only (dev builds; inert otherwise): the watchdog starts now, the workload once the shell opens.
  useBench(import.meta.env.DEV && !!bench, term, status, error, () => renderer.current);

  return (
    <>
      <div className="terminal-host" ref={host} />
      <div className="status-bar">
        {status}
        {error ? `: ${error}` : ""}
        {(status === "ended" || status === "lost" || status === "error") && (
          <button onClick={onExit} style={{ marginLeft: 8 }}>
            Back
          </button>
        )}
      </div>
    </>
  );
}
