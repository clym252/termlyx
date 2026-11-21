import { useEffect, useRef } from "react";
import type { ShellState } from "../terminal/useShell";
import { STEP_LABELS, STEPS, failureLine, stepMarks } from "./connectSteps";
import { RETRY_DELAYS_MS, type ReconnectState } from "./reconnect";

/** Who the card is about: the host's badge (as on the Hosts page), label and login. */
export interface SessionHeader {
  label: string;
  /** `user@host:port`. */
  address: string;
  badge: { text: string; colorIndex: number };
}

interface SessionStatusProps {
  state: ShellState;
  /** `null` for a local terminal or a DEV direct connect: no host to show. */
  header: SessionHeader | null;
  /**
   * The pane is the one the user is working in: when the session fails, the primary
   * action takes the focus (the terminal under it is dead, and xterm keeps Tab for the shell).
   */
  takeFocus?: boolean;
  /** The host key dialog is open for this session: the "Checking host key" line says it waits for the user. */
  waitingForYou?: boolean;
  /** Where the pane's automatic reconnecting stands; `idle` (the default) for a session that never dropped. */
  reconnect?: ReconnectState;
  /** The clock `reconnect.dueAt` is read against (ms); the countdown shows the time left. */
  now?: number;
  /** Opens a new session on the same target in this pane (Retry, Reconnect, Reconnect now). */
  onReconnect: () => void;
  /** Stops the automatic reconnecting (the card turns into "Connection lost"). */
  onStopReconnect?: () => void;
  /** Stops connecting and closes the pane. */
  onCancel: () => void;
  /** Closes the pane. */
  onClose: () => void;
}

/**
 * The pane's card for a session that is not simply running, centred over the
 * dimmed terminal: connecting (the steps and where it is), never opened
 * ("Could not connect", the failed step and why, Retry), reconnecting after
 * a drop ("Reconnecting…", the attempt, the countdown and steps), lost with
 * the connection ("Connection lost", why, Reconnect: retries used up,
 * cancelled, or not attempted), or ended by the remote shell ("Session
 * ended", with the exit status when known).
 */
export function SessionStatus({
  state,
  header,
  reconnect = { kind: "idle" },
  now = 0,
  takeFocus = false,
  waitingForYou = false,
  onReconnect,
  onStopReconnect,
  onCancel,
  onClose,
}: SessionStatusProps) {
  const primary = useRef<HTMLButtonElement>(null);
  // The reconnect state decides what a lost or failed session means: a retry under way, or a stop.
  const reconnecting = reconnect.kind === "waiting" || reconnect.kind === "trying";
  const stopped = reconnect.kind === "gaveUp" || reconnect.kind === "manual";
  const connecting = !reconnecting && !stopped && state.status === "connecting";
  const refused = !reconnecting && !stopped && state.status === "error";
  const lost = stopped || (!reconnecting && state.status === "lost");
  const failed = lost || refused;
  useEffect(() => {
    if (failed && takeFocus) primary.current?.focus();
  }, [failed, takeFocus]);

  if (state.status === "open") return null;

  // A step list only means something once a step has been reported (or for a host, where one is coming).
  const steps = reconnecting || ((connecting || refused) && (header !== null || state.step !== null));
  // Between attempts the steps are hollow, except the one the last attempt failed at.
  const attemptLive = state.status === "connecting";
  const marks = reconnecting
    ? stepMarks(attemptLive || state.status === "error" ? state.step : null, state.status === "error")
    : stepMarks(state.step, refused);
  const title = reconnecting
    ? "Reconnecting…"
    : connecting
      ? "Connecting…"
      : refused
        ? "Could not connect"
        : lost
          ? "Connection lost"
          : state.exitStatus != null
            ? `Session ended (exit status ${state.exitStatus})`
            : "Session ended";
  const secondsLeft = reconnect.kind === "waiting" ? Math.max(0, Math.ceil((reconnect.dueAt - now) / 1000)) : 0;
  // Why the connection is gone, when the step list does not already say.
  const reason = lost
    ? state.status === "error"
      ? failureLine(state.step, state.error ?? "")
      : (state.error ?? "Reconnecting was stopped.")
    : reconnecting && state.status === "lost"
      ? state.error
      : reconnecting && state.status === "error" && state.step === null
        ? failureLine(null, state.error ?? "")
        : undefined;

  const body = (
    <>
      {header && (
        <div className="session-host">
          <span
            className="host-badge"
            aria-hidden="true"
            style={{
              background: `var(--badge-${header.badge.colorIndex + 1}-bg)`,
              color: `var(--badge-${header.badge.colorIndex + 1}-fg)`,
            }}
          >
            {header.badge.text}
          </span>
          <span className="session-host-text">
            <span className="session-host-label">{header.label}</span>
            <span className="session-host-address">{header.address}</span>
          </span>
        </div>
      )}
      <p className="session-title">{title}</p>
      {reconnect.kind === "waiting" || reconnect.kind === "trying" ? (
        <p className="session-subtitle">attempt {reconnect.attempt} of {RETRY_DELAYS_MS.length}</p>
      ) : null}
      {/* Changes every second: kept out of the live region's announcements. */}
      {reconnect.kind === "waiting" && (
        <p className="session-subtitle" aria-live="off">
          next try in {secondsLeft}s
        </p>
      )}
      {steps && (
        <ol className="session-steps">
          {STEPS.map((step) => {
            const mark = marks[step];
            const text =
              mark === "failed"
                ? failureLine(step, state.error ?? "")
                : mark === "current" && step === "hostKey" && waitingForYou
                  ? `${STEP_LABELS[step]} — waiting for you`
                  : STEP_LABELS[step];
            return (
              <li key={step} data-mark={mark} aria-current={mark === "current" ? "step" : undefined}>
                <span className="session-step-icon" aria-hidden="true" />
                <span className="session-step-text">{text}</span>
              </li>
            );
          })}
        </ol>
      )}
      {/* A failure before any step was reported, or after the session was open, has no step line to carry the reason. */}
      {refused && !(steps && state.step !== null) && (
        <p className="session-detail">{failureLine(null, state.error ?? "")}</p>
      )}
      {reason !== undefined && <p className="session-detail">{reason}</p>}
    </>
  );

  return (
    <div className="session-overlay">
      <div className="session-card" data-tone={failed ? "failed" : undefined}>
        {failed ? <div role="alert">{body}</div> : <div role="status">{body}</div>}
        <div className="session-actions">
          {reconnecting && (
            <>
              <button type="button" onClick={onStopReconnect}>
                Cancel
              </button>
              <button type="button" className="primary" disabled={reconnect.kind === "trying"} onClick={onReconnect}>
                Reconnect now
              </button>
            </>
          )}
          {connecting && (
            <button type="button" onClick={onCancel}>
              Cancel
            </button>
          )}
          {failed && (
            <button type="button" className="primary" ref={primary} onClick={onReconnect}>
              {refused ? "Retry" : "Reconnect"}
            </button>
          )}
          {!connecting && !reconnecting && (
            <button type="button" onClick={onClose}>
              Close
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
