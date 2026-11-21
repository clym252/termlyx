import { invoke } from "@tauri-apps/api/core";
import {
  GATE_MAX_STALL_MS,
  GATE_MIN_MBPS,
  MarkerScanner,
  mbPerSec,
  passesGate,
} from "./benchMetrics";
import { onTerminalWrite } from "./outputTap";
import { StallMonitor } from "./stallMonitor";

/**
 * Throughput bench (Phase 0 spike). Only ever started when Rust reports
 * `TETHER_BENCH=1`; see `src-tauri/src/bench.rs`.
 *
 * Drives the remote shell through the terminal like a user would:
 *   1. generate /tmp/big (~135 MB of base64) and wait for READY
 *   2. print START, `cat /tmp/big`, print END
 * and measures bytes/s between the START and END markers as parsed by xterm.
 */

/** Connection details returned by the `bench_config` command. */
export interface BenchConfig {
  host: string;
  port: number;
  username: string;
  password: string;
}

// '@' is not in the base64 alphabet, so these can never appear in the payload.
// The shell command spells them with printf specifiers, so the echoed command
// line does not contain them either.
const READY = "@@TETHER_BENCH_READY@@";
const START = "@@TETHER_BENCH_START@@";
const END = "@@TETHER_BENCH_END@@";
const mark = (name: "READY" | "START" | "END") =>
  `printf '@@TETHER_%s_%s@@\\n' BENCH ${name}`;

const GENERATE =
  `head -c 100000000 /dev/urandom | base64 > /tmp/big; ${mark("READY")}\n`;
const RUN = `${mark("START")}; cat /tmp/big; ${mark("END")}\n`;

const QUIET_BEFORE_COMMAND_MS = 700;
const TIMEOUT_MS = 180_000;

export interface BenchResult {
  ok: boolean;
  error?: string;
  renderer: string;
  bytes: number;
  durationMs: number;
  mbps: number;
  maxStallMs: number;
  stallSource: string;
  maxLongTaskMs: number;
  longTaskSupported: boolean;
  maxRafGapMs: number;
  /**
   * Frames delivered between the markers, and the latency from a frame
   * arriving off IPC to xterm's write callback (queue wait plus parse).
   */
  frames: number;
  avgFrameBytes: number;
  writeLatencyMsTotal: number;
  maxWriteLatencyMs: number;
  gate: { minMbps: number; maxStallMs: number; pass: boolean };
  userAgent: string;
}

export interface BenchHandle {
  /** The shell is open: start the workload. Safe to call more than once. */
  begin(term: BenchTerm): void;
  /** Abort and report a failure (e.g. the SSH connection failed). Works before `begin`. */
  fail(reason: string): void;
  dispose(): void;
}

/** The bit of xterm the bench types into. `input` fires onData like a keystroke. */
export interface BenchTerm {
  input(data: string): void;
}

/**
 * Creates the bench for this app run. The watchdog starts immediately, so a
 * connect/auth failure (the shell never opens) still ends in a report and an
 * app exit. The result goes to Rust (`bench_report`), which writes it and
 * exits the app. Exactly one report is sent.
 */
export function createBench(
  getRenderer: () => string,
  timeoutMs = TIMEOUT_MS,
): BenchHandle {
  type Phase = "connecting" | "prompt" | "generating" | "measuring" | "done";
  let phase: Phase = "connecting";
  let term: BenchTerm | null = null;
  let off: (() => void) | null = null;
  const stalls = new StallMonitor();
  const ready = new MarkerScanner(READY);
  const start = new MarkerScanner(START);
  const end = new MarkerScanner(END);
  let startEnd = -1;
  let startAt = 0;
  let frames = 0;
  let writeLatencyMsTotal = 0;
  let maxWriteLatencyMs = 0;
  let quiet: ReturnType<typeof setTimeout> | undefined;

  const cleanup = () => {
    clearTimeout(quiet);
    clearTimeout(watchdog);
    off?.();
    off = null;
    stalls.stop();
  };

  const finish = (result: Partial<BenchResult> & { ok: boolean }) => {
    if (phase === "done") return;
    phase = "done";
    cleanup();
    const full: BenchResult = {
      renderer: getRenderer(),
      bytes: 0,
      durationMs: 0,
      mbps: 0,
      maxStallMs: 0,
      stallSource: "",
      maxLongTaskMs: 0,
      longTaskSupported: false,
      maxRafGapMs: 0,
      frames: 0,
      avgFrameBytes: 0,
      writeLatencyMsTotal: 0,
      maxWriteLatencyMs: 0,
      gate: { minMbps: GATE_MIN_MBPS, maxStallMs: GATE_MAX_STALL_MS, pass: false },
      userAgent: navigator.userAgent,
      ...result,
    };
    Promise.resolve(invoke("bench_report", { json: JSON.stringify(full) })).catch((e) =>
      console.error("bench_report failed", e),
    );
  };

  const watchdog = setTimeout(
    () => finish({ ok: false, error: `timed out in phase "${phase}"` }),
    timeoutMs,
  );

  const armPrompt = () => {
    clearTimeout(quiet);
    quiet = setTimeout(() => {
      phase = "generating";
      term?.input(GENERATE);
    }, QUIET_BEFORE_COMMAND_MS);
  };

  const onWrite = (chunk: Uint8Array, parsedAt: number, submittedAt: number) => {
    switch (phase) {
      case "prompt":
        armPrompt(); // wait for the banner/prompt to go quiet
        break;
      case "generating":
        if (ready.push(chunk)) {
          phase = "measuring";
          term?.input(RUN);
        }
        break;
      case "measuring": {
        if (startEnd >= 0) {
          frames++;
          const latency = parsedAt - submittedAt;
          writeLatencyMsTotal += latency;
          if (latency > maxWriteLatencyMs) maxWriteLatencyMs = latency;
        }
        const s = startEnd < 0 ? start.push(chunk) : null;
        const e = end.push(chunk);
        if (s) {
          startEnd = s.end;
          startAt = parsedAt;
          stalls.reset();
        }
        if (e && startEnd >= 0) {
          const bytes = e.start - startEnd;
          const durationMs = parsedAt - startAt;
          const snap = stalls.snapshot();
          const mbps = mbPerSec(bytes, durationMs);
          finish({
            ok: true,
            bytes,
            durationMs,
            mbps,
            maxStallMs: snap.maxStallMs,
            stallSource: snap.source,
            maxLongTaskMs: snap.maxLongTaskMs,
            longTaskSupported: snap.longTaskSupported,
            maxRafGapMs: snap.maxRafGapMs,
            frames,
            avgFrameBytes: frames ? Math.round(bytes / frames) : 0,
            writeLatencyMsTotal,
            maxWriteLatencyMs,
            gate: {
              minMbps: GATE_MIN_MBPS,
              maxStallMs: GATE_MAX_STALL_MS,
              pass: passesGate(mbps, snap.maxStallMs),
            },
          });
        }
        break;
      }
      default:
        break;
    }
  };

  return {
    begin(t) {
      if (phase !== "connecting") return;
      term = t;
      phase = "prompt";
      stalls.start();
      off = onTerminalWrite(onWrite);
      armPrompt();
    },
    fail: (reason) => finish({ ok: false, error: reason }),
    dispose: () => {
      if (phase !== "done") {
        phase = "done";
        cleanup();
      }
    },
  };
}
