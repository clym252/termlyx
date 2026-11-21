import { useEffect, useRef } from "react";
import type { ShellStatus } from "../terminal/useShell";
import { createBench, type BenchHandle, type BenchTerm } from "./bench";

type UseBench = (
  enabled: boolean,
  term: BenchTerm | null,
  status: ShellStatus,
  error: string | undefined,
  getRenderer: () => string,
) => void;

/**
 * Drives the throughput bench from the terminal view. Inert unless `enabled`
 * (the app was launched with TETHER_BENCH=1). The watchdog starts at mount,
 * so a connect/auth failure still produces a failure report and exits.
 *
 * Production builds get a no-op, so the bench harness is not bundled.
 */
export const useBench: UseBench = import.meta.env.DEV ? useBenchDev : () => {};

function useBenchDev(
  enabled: boolean,
  term: BenchTerm | null,
  status: ShellStatus,
  error: string | undefined,
  getRenderer: () => string,
): void {
  const handle = useRef<BenchHandle | null>(null);
  const renderer = useRef(getRenderer);
  renderer.current = getRenderer;

  useEffect(() => {
    if (!enabled) return;
    const h = createBench(() => renderer.current());
    handle.current = h;
    return () => {
      h.dispose();
      handle.current = null;
    };
  }, [enabled]);

  useEffect(() => {
    if (enabled && term && status === "open") handle.current?.begin(term);
  }, [enabled, term, status]);

  useEffect(() => {
    if (enabled && (status === "error" || status === "ended" || status === "lost")) {
      handle.current?.fail(`shell ${status}${error ? `: ${error}` : ""}`);
    }
  }, [enabled, status, error]);
}
