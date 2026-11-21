import type { ConnectStep } from "../api/types";

/** Wait before attempt 1, 2, ... 5 of an automatic reconnect. */
export const RETRY_DELAYS_MS = [1000, 2000, 4000, 8000, 15000] as const;

/**
 * How long a reconnected session must stay open before the retry count
 * starts over: a connection that opens and drops within seconds keeps
 * counting towards the 5 attempts instead of looping forever.
 */
export const STABLE_OPEN_MS = 10_000;

/** Where a pane's reconnecting stands. `idle`: connected, or never was (a first connect is not retried). */
export type ReconnectState =
  | { kind: "idle" }
  /** Attempt `attempt` (1-based) starts at `dueAt` (ms, the clock of the events' `now`). */
  | { kind: "waiting"; attempt: number; dueAt: number }
  | { kind: "trying"; attempt: number }
  /** Retries used up, cancelled, or stopped by a failure that retrying cannot fix. */
  | { kind: "gaveUp" }
  /** Not retried by itself (dropped in the background, or the vault locked): waits for the user. */
  | { kind: "manual" };

export type ReconnectEvent =
  /** An open session dropped. `whileActive`: the app was in front (and the vault unlocked) when it did. */
  | { type: "lost"; whileActive: boolean; now: number }
  | { type: "opened" }
  | { type: "failed"; now: number; fatal: boolean }
  | { type: "tick"; now: number }
  | { type: "reconnectNow" }
  | { type: "cancel" }
  | { type: "locked" };

/** The state after attempt `n` failed (or the drop, for `n` = 0): wait for attempt `n + 1`, or give up. */
function afterAttempt(n: number, now: number): ReconnectState {
  return n >= RETRY_DELAYS_MS.length
    ? { kind: "gaveUp" }
    : { kind: "waiting", attempt: n + 1, dueAt: now + RETRY_DELAYS_MS[n] };
}

/** Pure: all timing comes in through the events' `now`. States that do not change are returned as they are. */
export function reconnectReducer(s: ReconnectState, e: ReconnectEvent): ReconnectState {
  switch (e.type) {
    case "lost":
      // A drop during an attempt (it opened and went, or went before it opened) is that attempt failing.
      if (s.kind === "trying") return e.whileActive ? afterAttempt(s.attempt, e.now) : { kind: "manual" };
      if (s.kind !== "idle") return s;
      return e.whileActive ? afterAttempt(0, e.now) : { kind: "manual" };
    case "opened":
      return s.kind === "idle" ? s : { kind: "idle" };
    case "failed":
      if (s.kind !== "trying") return s;
      return e.fatal ? { kind: "gaveUp" } : afterAttempt(s.attempt, e.now);
    case "tick":
      return s.kind === "waiting" && e.now >= s.dueAt ? { kind: "trying", attempt: s.attempt } : s;
    case "reconnectNow":
      if (s.kind === "waiting") return { kind: "trying", attempt: s.attempt };
      return s.kind === "gaveUp" || s.kind === "manual" ? { kind: "trying", attempt: 1 } : s;
    case "cancel":
      return s.kind === "waiting" || s.kind === "trying" ? { kind: "gaveUp" } : s;
    case "locked":
      return s.kind === "waiting" || s.kind === "trying" ? { kind: "manual" } : s;
  }
}

/** Failures that trying again cannot fix: the server's key was refused, or the login was. */
const FATAL_CODES: ReadonlySet<string> = new Set([
  "hostKeyRejected",
  "hostKeyUnverifiable",
  "hostKeyEncoding",
  "authFailed",
]);

/**
 * Whether a failed attempt ends the automatic retries. By error code (the
 * backend's `SshError` codes, see `state.rs`): a rejected, unverifiable or
 * malformed host key, or a rejected login. By step: anything but a plain
 * transport failure (`connect`) at `hostKey`, where a changed key surfaces
 * as a prompt and its refusal. A `connect` failure at any step (timeout,
 * refused, reset) is the network and worth retrying.
 */
export function isFatalFailure(step: ConnectStep | null, code: string | undefined): boolean {
  if (code !== undefined && FATAL_CODES.has(code)) return true;
  return step === "hostKey" && code !== "connect";
}
