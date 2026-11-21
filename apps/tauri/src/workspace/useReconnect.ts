import { useCallback, useEffect, useRef, useState } from "react";
import type { ShellState } from "../terminal/useShell";
import {
  STABLE_OPEN_MS,
  isFatalFailure,
  reconnectReducer,
  type ReconnectEvent,
  type ReconnectState,
} from "./reconnect";

/** How often a waiting pane checks its clock (and refreshes its countdown). */
const TICK_MS = 250;

const IDLE: ReconnectState = { kind: "idle" };

interface UseReconnectOptions {
  /** The pane's session, as `useShell` reports it. */
  state: ShellState;
  /** The app is in front (`useAppActive`). */
  active: boolean;
  /** When the app last went to the background, while that still counts (`useAppActive`). */
  inactiveSince: number | null;
  /** The vault is locked: no attempt may start. */
  locked: boolean;
  /** Start a new attempt: open another session on the pane's target. */
  onAttempt: () => void;
  /** Retrying has stopped (cancelled, given up, or left to the user): abandon any attempt under way. */
  onStop?: () => void;
}

export interface Reconnect {
  reconnect: ReconnectState;
  /** The clock the countdown reads (ms), refreshed while waiting. */
  now: number;
  /** Start the next attempt at once. */
  reconnectNow: () => void;
  /** Stop retrying. */
  cancel: () => void;
}

/**
 * Drives the reconnect reducer from a pane's session: a drop of an open
 * session while the app is in front schedules attempts; ended sessions and
 * first-connect failures never do. `onAttempt` is called as the state enters
 * `trying` (and `onStop` as it enters `gaveUp` or `manual`), in the same batch
 * as that state, so the pane changes both at once.
 */
export function useReconnect({
  state,
  active,
  inactiveSince,
  locked,
  onAttempt,
  onStop,
}: UseReconnectOptions): Reconnect {
  // The reducer runs outside React's updaters so that `onAttempt` fires once per transition.
  const current = useRef<ReconnectState>(IDLE);
  const [reconnect, setReconnect] = useState<ReconnectState>(IDLE);
  const [now, setNow] = useState(() => Date.now());
  const attempt = useRef(onAttempt);
  attempt.current = onAttempt;
  const stop = useRef(onStop);
  stop.current = onStop;
  // What the next session event is judged by.
  const context = useRef({ active, inactiveSince, locked });
  context.current = { active, inactiveSince, locked };

  const send = useCallback((event: ReconnectEvent) => {
    const next = reconnectReducer(current.current, event);
    if (next === current.current) return;
    current.current = next;
    setReconnect(next);
    setNow(Date.now());
    if (next.kind === "trying") attempt.current();
    else if (next.kind === "gaveUp" || next.kind === "manual") stop.current?.();
  }, []);

  useEffect(() => {
    const t = Date.now();
    switch (state.status) {
      case "open": {
        if (current.current.kind === "idle") break;
        // A reconnected session counts as back only once it has stayed up; a drop before that is the attempt failing.
        const stable = setTimeout(() => send({ type: "opened" }), STABLE_OPEN_MS);
        return () => clearTimeout(stable);
      }
      case "lost": {
        const c = context.current;
        send({ type: "lost", whileActive: c.active && c.inactiveSince === null && !c.locked, now: t });
        break;
      }
      case "error":
        // Only a failed attempt counts: a first connect has no reconnect state to fail.
        send({ type: "failed", now: t, fatal: isFatalFailure(state.step, state.errorCode) });
        break;
    }
  }, [state, send]);

  const status = useRef(state.status);
  status.current = state.status;
  useEffect(() => {
    // A session that just reopened stays: locking does not close what is working.
    if (locked && status.current !== "open") send({ type: "locked" });
  }, [locked, send]);

  // The countdown runs only while the app is in front and the vault open.
  const waiting = reconnect.kind === "waiting";
  useEffect(() => {
    if (!waiting || !active || locked) return;
    const id = setInterval(() => {
      const t = Date.now();
      setNow(t);
      send({ type: "tick", now: t });
    }, TICK_MS);
    return () => clearInterval(id);
  }, [waiting, active, locked, send]);

  const reconnectNow = useCallback(() => send({ type: "reconnectNow" }), [send]);
  const cancel = useCallback(() => send({ type: "cancel" }), [send]);
  return { reconnect, now, reconnectNow, cancel };
}
