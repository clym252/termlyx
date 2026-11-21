import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { TerminalView } from "../terminal/Terminal";
import type { ConnectInput } from "../terminal/useShell";
import type { BenchConfig } from "./bench";
import { ConnectForm } from "./ConnectForm";
import { ThroughputOverlay } from "./ThroughputOverlay";

/**
 * `null` unless this is a dev build of the UI and the backend was built with
 * the `bench` cargo feature and launched with TETHER_BENCH=1 (see
 * src-tauri/src/bench.rs).
 */
export async function loadBenchConfig(): Promise<BenchConfig | null> {
  if (!import.meta.env.DEV || import.meta.env.VITE_TETHER_API === "mock") return null;
  try {
    return (await invoke<BenchConfig | null>("bench_config")) ?? null;
  } catch {
    return null;
  }
}

export function benchParams(cfg: BenchConfig): ConnectInput {
  return { host: cfg.host, port: cfg.port, username: cfg.username, password: cfg.password };
}

/**
 * DEV only. The Phase 0 connect form and terminal, which need a backend
 * built with `--features dev-connect` (the `open_shell` command). Task 14
 * replaces it with the real workspace; the throughput bench starts here
 * directly, without the vault (see App).
 */
export function DevTerminal({ bench }: { bench?: BenchConfig }) {
  const [params, setParams] = useState<ConnectInput | null>(bench ? benchParams(bench) : null);
  return (
    <>
      {params ? (
        <TerminalView params={params} onExit={() => setParams(null)} bench={bench} />
      ) : (
        <ConnectForm onConnect={setParams} />
      )}
      <ThroughputOverlay />
    </>
  );
}
