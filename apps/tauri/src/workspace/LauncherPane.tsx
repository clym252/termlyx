import { useCallback, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { api } from "../api";
import type { Host, SessionTarget } from "../api/types";
import { sortHosts } from "../hosts/model";
import { IS_MOBILE } from "../platform";
import { useVaultChanged } from "../sync/vaultChanged";
import { describeError } from "../ui/errors";
import { paneLabel, type PaneSpec } from "./model";
import { PaneHeader, type PaneHeaderControls } from "./PaneHeader";
import { usePaneDrop } from "./paneDrag";
import { SHORTCUT_LABELS } from "./shortcuts";
import { LOCAL_TITLE, localTarget } from "./local";

/** Before anything is opened: Local terminal (not on iOS), or a host. */
export function Launcher({
  onPick,
  heading = "New tab",
  onCancel,
}: {
  onPick: (target: SessionTarget, title: string) => void;
  heading?: string;
  /** Offers a way out (a pane's launcher; a tab has its close button). */
  onCancel?: () => void;
}) {
  const [hosts, setHosts] = useState<Host[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(async () => {
    try {
      setHosts(sortHosts(await api.listEntities("host")));
      setError(null);
    } catch (e) {
      setError(describeError(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);
  useVaultChanged(load);
  return (
    <div className="launcher" data-loaded={hosts ? "true" : undefined}>
      <h2>{heading}</h2>
      {!IS_MOBILE && (
        <>
          <button type="button" className="primary" onClick={() => onPick(localTarget(), LOCAL_TITLE)}>
            Local terminal
          </button>
          <span className="muted small">{SHORTCUT_LABELS.localTerminal} opens one from anywhere.</span>
        </>
      )}
      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}
      {hosts && hosts.length > 0 && (
        <ul className="launcher-hosts" aria-label="Connect to a host">
          {hosts.map((h) => (
            <li key={h.id}>
              <button type="button" className="host-row" onClick={() => onPick({ kind: "host", hostId: h.id }, h.label)}>
                <span className="host-label">{h.label}</span>
                <span className="host-address">{h.address}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {onCancel && (
        <button type="button" onClick={onCancel}>
          Cancel
        </button>
      )}
    </div>
  );
}

interface LauncherPaneProps {
  pane: PaneSpec;
  /** The pane's stable DOM home, moved between layout slots (see Workspace). */
  host: HTMLElement;
  active: boolean;
  /** The pane shares its tab with others: it gets a header and focus styling. */
  inSplit: boolean;
  onActivate: () => void;
  onClose: () => void;
  /** The header's menu and maximize state (shown with `inSplit`). */
  header: PaneHeaderControls;
  onPick: (target: SessionTarget, title: string) => void;
}

/**
 * A split pane that has not opened anything yet: the host list in place.
 * Rendered into `host` like TerminalPane, so it moves with the layout.
 */
export function LauncherPane({ pane, host, active, inSplit, onActivate, onClose, header, onPick }: LauncherPaneProps) {
  const drop = usePaneDrop(pane.id, header.onSwap);
  return createPortal(
    <div
      className="terminal-pane"
      data-pane-id={pane.id}
      data-active={active ? "true" : "false"}
      data-split={inSplit ? "true" : "false"}
      data-launcher="true"
      onMouseDownCapture={onActivate}
      onFocusCapture={onActivate}
      {...drop}
    >
      {inSplit && <PaneHeader paneId={pane.id} label={paneLabel(pane)} active={active} onActivate={onActivate} {...header} />}
      <Launcher heading="Open in this pane" onPick={onPick} onCancel={onClose} />
    </div>,
    host,
  );
}
