import { Fragment, useRef, useState, type TouchEvent } from "react";
import type { ShellState } from "../terminal/useShell";
import type { ReconnectState } from "./reconnect";

/** Where a pane's session stands, in one word (the session card says the rest). */
export type PaneStatus = "connecting" | "open" | "reconnecting" | "lost" | "failed" | "ended";

/** A pane's status from its shell and its reconnecting, as its session card reads them. */
export function paneStatus(state: ShellState, reconnect: ReconnectState): PaneStatus {
  if (state.status === "open") return "open";
  if (reconnect.kind === "waiting" || reconnect.kind === "trying") return "reconnecting";
  if (reconnect.kind === "gaveUp" || reconnect.kind === "manual") return "lost";
  switch (state.status) {
    case "connecting":
      return "connecting";
    case "error":
      return "failed";
    case "lost":
      return "lost";
    case "ended":
      return "ended";
  }
}

export const STATUS_WORDS: Record<PaneStatus, string> = {
  connecting: "Connecting…",
  open: "Connected",
  reconnecting: "Reconnecting…",
  lost: "Connection lost",
  failed: "Could not connect",
  ended: "Session ended",
};

export interface PhoneSession {
  tabId: string;
  label: string;
  /** Unknown until the pane reports (a tab still picking its target). */
  status: PaneStatus | null;
  /** A split tab's panes, listed under its row; absent for a one-pane tab. */
  panes?: PhonePane[];
}

export interface PhonePane {
  paneId: string;
  label: string;
  /** Null for a launcher pane (nothing is connecting there). */
  status: PaneStatus | null;
  /** The tab's focused pane, the one the phone shows. */
  active: boolean;
}

/** A row swiped this far to the left shows its Close; as far back hides it. */
const REVEAL_PX = 40;

interface PhoneTerminalsProps {
  sessions: PhoneSession[];
  /** Puts the session's terminal on screen. */
  onShow(tabId: string): void;
  /** Puts one pane of a split tab on screen. */
  onShowPane(tabId: string, paneId: string): void;
  /** Ends the whole tab: every pane of a split one. */
  onClose(tabId: string): void;
  /** Ends one pane of a split tab, leaving the others. */
  onClosePane(paneId: string): void;
}

/**
 * The open sessions on a phone (no tab strip there): label and status per
 * session; a tap shows it, a swipe to the left reveals Close (as does
 * moving the focus onto it). A split tab's panes follow its row, indented;
 * a tap shows that pane, and each has its own Close. The tab row's Close
 * ends the whole tab, which its label says.
 */
export function PhoneTerminals({ sessions, onShow, onShowPane, onClose, onClosePane }: PhoneTerminalsProps) {
  // The row (a tab's or a pane's id) whose Close shows.
  const [revealed, setRevealed] = useState<string | null>(null);
  // The current touch on a row: where it started, and whether it swiped (then the tap that ends it is no tap).
  const touch = useRef<{ x: number; y: number; swiped: boolean } | null>(null);

  const onTouchStart = (e: TouchEvent) => {
    const t = e.touches[0];
    touch.current = t ? { x: t.clientX, y: t.clientY, swiped: false } : null;
  };
  const onTouchMove = (rowId: string, e: TouchEvent) => {
    const t = e.touches[0];
    const start = touch.current;
    if (!t || !start) return;
    const dx = t.clientX - start.x;
    if (Math.abs(t.clientY - start.y) > Math.abs(dx)) return;
    if (dx < -REVEAL_PX) {
      start.swiped = true;
      setRevealed(rowId);
    } else if (dx > REVEAL_PX) {
      start.swiped = true;
      setRevealed((r) => (r === rowId ? null : r));
    }
  };

  // A tap on a row whose Close shows hides it again; a tap ending a swipe is no tap.
  const tapped = (rowId: string, show: () => void) => {
    const swiped = touch.current?.swiped;
    touch.current = null;
    if (swiped) return;
    if (revealed === rowId) setRevealed(null);
    else show();
  };

  return (
    <section className="page phone-terminals" aria-labelledby="phone-terminals-title">
      <header className="page-head">
        <h1 id="phone-terminals-title">Terminals</h1>
      </header>
      {sessions.length === 0 ? (
        <p className="muted empty">No open sessions.</p>
      ) : (
        <ul className="phone-sessions" aria-label="Open sessions">
          {sessions.map((s) => {
            const status = s.status ? STATUS_WORDS[s.status] : "";
            return (
              <Fragment key={s.tabId}>
                <li className="phone-session" data-revealed={revealed === s.tabId ? "true" : undefined}>
                  <button
                    type="button"
                    className="phone-session-main"
                    aria-label={status ? `${s.label}, ${status}` : s.label}
                    onTouchStart={onTouchStart}
                    onTouchMove={(e) => onTouchMove(s.tabId, e)}
                    onClick={() => tapped(s.tabId, () => onShow(s.tabId))}
                  >
                    <span className="phone-session-label">{s.label}</span>
                    <span className="phone-session-status" data-status={s.status ?? undefined}>
                      {status}
                    </span>
                  </button>
                  <button
                    type="button"
                    className="phone-session-close danger"
                    aria-label={s.panes ? `Close tab (${s.panes.length} sessions)` : `Close session ${s.label}`}
                    onFocus={() => setRevealed(s.tabId)}
                    onClick={() => onClose(s.tabId)}
                  >
                    Close
                  </button>
                </li>
                {s.panes?.map((p) => {
                  const paneStatus = p.status ? STATUS_WORDS[p.status] : "";
                  return (
                    <li key={p.paneId} className="phone-pane" data-revealed={revealed === p.paneId ? "true" : undefined}>
                      <button
                        type="button"
                        className="phone-pane-main"
                        aria-label={paneStatus ? `${p.label}, ${paneStatus}` : p.label}
                        aria-current={p.active ? "true" : undefined}
                        onTouchStart={onTouchStart}
                        onTouchMove={(e) => onTouchMove(p.paneId, e)}
                        onClick={() => tapped(p.paneId, () => onShowPane(s.tabId, p.paneId))}
                      >
                        <span className="phone-session-label">{p.label}</span>
                        <span className="phone-session-status" data-status={p.status ?? undefined}>
                          {paneStatus}
                        </span>
                      </button>
                      <button
                        type="button"
                        className="phone-session-close danger"
                        aria-label={`Close session ${p.label}`}
                        onFocus={() => setRevealed(p.paneId)}
                        onClick={() => onClosePane(p.paneId)}
                      >
                        Close
                      </button>
                    </li>
                  );
                })}
              </Fragment>
            );
          })}
        </ul>
      )}
    </section>
  );
}
