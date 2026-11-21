import { useEffect, useRef, useState } from "react";
import { api as appApi } from "../api";
import type { ActiveWork, ForwardRule, Host, TetherApi, Unlisten } from "../api/types";
import { transferStore } from "../sftp/transfers";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { APP_NAME } from "../app/brand";

type Asking = { work: ActiveWork; hosts: Map<string, string>; rules: Map<string, string> } | { error: string };

function hostLabel(hosts: Map<string, string>, id: string | null): string | null {
  return id === null ? null : (hosts.get(id) ?? null);
}

/**
 * Closing the window with live sessions, running transfers or running port
 * forwards asks first,
 * listing them (Cancel is the default); with nothing live it just closes.
 * With transfers running, "Cancel transfers" stops them and keeps the app
 * (and its sessions and forwards) open.
 * If the list cannot be read, it still asks rather than closing silently.
 *
 * The window close request is the one entry point: on macOS the app
 * menu's "Quit Tether" (Cmd+Q) and "Close Window" ask every window to close
 * the same way (src-tauri/src/menu.rs). Quit from the Dock menu, logout and
 * shutdown terminate the app directly and are not guarded.
 */
export function QuitGuard({ api = appApi }: { api?: TetherApi }) {
  const [asking, setAsking] = useState<Asking | null>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    let live = true;
    let off: Unlisten | undefined;
    void api
      .onCloseRequested(async (event) => {
        event.preventDefault();
        let work: ActiveWork;
        try {
          work = await api.activeWork();
        } catch (e) {
          if (live) setAsking({ error: describeError(e) });
          return;
        }
        if (work.sessions.length === 0 && work.transfers.length === 0 && work.forwards.length === 0) {
          await api.destroyWindow();
          return;
        }
        // Host and rule labels for the list; the vault may be locked (then ids are left out).
        let hosts = new Map<string, string>();
        let rules = new Map<string, string>();
        try {
          hosts = new Map((await api.listEntities("host")).map((h: Host) => [h.id, h.label]));
          rules = new Map((await api.listEntities("forward_rule")).map((r: ForwardRule) => [r.id, r.label]));
        } catch {
          // Labels are a nicety.
        }
        if (live) setAsking({ work, hosts, rules });
      })
      .then((unlisten) => {
        if (live) off = unlisten;
        else unlisten();
      });
    return () => {
      live = false;
      off?.();
    };
  }, [api]);

  if (!asking) return null;
  const cancel = () => setAsking(null);
  const cancelTransfers = (jobIds: readonly string[]) => {
    setAsking(null);
    // A folder still being queued stops too.
    transferStore.stopQueuing();
    for (const id of jobIds) {
      api.transferCancel(id).catch((e: unknown) => console.debug("transfer_cancel failed", e));
    }
  };
  const quit = () => {
    setAsking(null);
    api.destroyWindow().catch((e: unknown) => console.debug("destroy window failed", e));
  };

  return (
    <Dialog title={`Quit ${APP_NAME}?`} onClose={cancel} initialFocus={cancelRef}>
      <div className="dialog-content">
        {"error" in asking ? (
          <p role="alert">
            Could not check for open sessions, transfers and forwards ({asking.error}). Quitting may end them.
          </p>
        ) : (
          <>
            <p>Quitting ends these:</p>
            {asking.work.sessions.length > 0 && (
              <div>
                <h3 className="small muted">Sessions</h3>
                <ul className="quit-list" aria-label="Open sessions">
                  {asking.work.sessions.map((s) => (
                    <li key={s.id}>{s.title}</li>
                  ))}
                </ul>
              </div>
            )}
            {asking.work.transfers.length > 0 && (
              <div>
                <h3 className="small muted">Transfers</h3>
                <ul className="quit-list" aria-label="Running transfers">
                  {asking.work.transfers.map((t) => {
                    const host = hostLabel(asking.hosts, t.hostId);
                    return (
                      <li key={t.jobId} className="mono small">
                        {t.direction === "upload" ? `${t.local} → ${host ? `${host}:` : ""}${t.remote}` : `${host ? `${host}:` : ""}${t.remote} → ${t.local}`}
                      </li>
                    );
                  })}
                </ul>
              </div>
            )}
            {asking.work.forwards.length > 0 && (
              <div>
                <h3 className="small muted">Port forwards</h3>
                <ul className="quit-list" aria-label="Running port forwards">
                  {asking.work.forwards.map((f) => {
                    const host = hostLabel(asking.hosts, f.hostId);
                    const label = asking.rules.get(f.ruleId) ?? "Port forward";
                    const bound = f.status.state === "active" ? ` (${f.status.detail.bound})` : "";
                    return (
                      <li key={f.ruleId}>
                        {label}
                        {host ? ` on ${host}` : ""}
                        <span className="mono small">{bound}</span>
                      </li>
                    );
                  })}
                </ul>
              </div>
            )}
          </>
        )}
      </div>
      <div className="dialog-actions">
        <button type="button" ref={cancelRef} onClick={cancel}>
          Cancel
        </button>
        {"work" in asking && asking.work.transfers.length > 0 && (
          <button type="button" onClick={() => cancelTransfers(asking.work.transfers.map((t) => t.jobId))}>
            Cancel transfers
          </button>
        )}
        <button type="button" className="danger" onClick={quit}>
          Quit anyway
        </button>
      </div>
    </Dialog>
  );
}
