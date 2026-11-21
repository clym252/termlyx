import { useCallback, useEffect, useId, useState, type FormEvent } from "react";
import { api } from "../api";
import type { Device } from "../api/types";
import { ConfirmDialog } from "../ui/Dialog";
import { SyncProblem } from "./SyncProblem";
import { formatTime, problemFromError, type Problem } from "./status";

/**
 * The account's devices: name, platform, last seen, this device marked.
 * Rename in place; revoke after a confirmation (not offered for this device:
 * "Turn off sync on this device" is the way to leave). Reloads when
 * `refreshKey` changes (a sync finished).
 */
export function DevicesList({ refreshKey }: { refreshKey: string }) {
  const titleId = useId();
  const [devices, setDevices] = useState<Device[] | null>(null);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [renaming, setRenaming] = useState<{ id: string; name: string } | null>(null);
  const [revoking, setRevoking] = useState<Device | null>(null);
  const [revokeError, setRevokeError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setDevices(await api.sync.devices());
      setProblem(null);
    } catch (e) {
      setProblem(problemFromError(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load, refreshKey]);

  async function saveName(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (!renaming) return;
    const name = renaming.name.trim();
    if (!name) {
      setProblem({ message: "Enter a name for the device.", hint: null, severe: false });
      return;
    }
    setBusy(true);
    try {
      await api.sync.renameDevice(renaming.id, name);
      setRenaming(null);
      await load();
    } catch (err) {
      setProblem(problemFromError(err));
    } finally {
      setBusy(false);
    }
  }

  async function revoke() {
    if (!revoking) return;
    setBusy(true);
    setRevokeError(null);
    try {
      await api.sync.revokeDevice(revoking.id);
      setRevoking(null);
      await load();
    } catch (err) {
      setRevokeError(problemFromError(err).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="list-section">
      <h3 id={titleId}>Devices</h3>
      {problem && <SyncProblem problem={problem} />}
      {devices && (
        <ul className="item-list" aria-labelledby={titleId}>
          {devices.map((d) => (
            <li key={d.id} className="item-row">
              {renaming?.id === d.id ? (
                <form className="row grow" onSubmit={(e) => void saveName(e)}>
                  <input
                    className="grow"
                    aria-label="Device name"
                    value={renaming.name}
                    autoFocus
                    onChange={(e) => setRenaming({ id: d.id, name: e.target.value })}
                    onKeyDown={(e) => {
                      if (e.key === "Escape") {
                        e.stopPropagation();
                        setRenaming(null);
                        setProblem(null);
                      }
                    }}
                  />
                  <button type="submit" className="primary" disabled={busy}>
                    Save
                  </button>
                  <button type="button" onClick={() => setRenaming(null)} disabled={busy}>
                    Cancel
                  </button>
                </form>
              ) : (
                <>
                  <div className="item-main">
                    <span>
                      {d.name}
                      {d.current && <span className="badge">This device</span>}
                    </span>
                    <span className="item-meta">
                      <span>{d.platform}</span>
                      {!d.current && <span>Last seen {formatTime(d.lastSeen)}</span>}
                    </span>
                  </div>
                  <div className="item-actions">
                    <button
                      type="button"
                      aria-label={`Rename ${d.name}`}
                      onClick={() => setRenaming({ id: d.id, name: d.name })}
                    >
                      Rename
                    </button>
                    {!d.current && (
                      <button
                        type="button"
                        className="danger-text"
                        aria-label={`Revoke ${d.name}`}
                        onClick={() => {
                          setRevokeError(null);
                          setRevoking(d);
                        }}
                      >
                        Revoke
                      </button>
                    )}
                  </div>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
      {revoking && (
        <ConfirmDialog
          title="Revoke device"
          confirmLabel="Revoke"
          danger
          busy={busy}
          error={revokeError}
          onConfirm={() => void revoke()}
          onCancel={() => setRevoking(null)}
        >
          <p>
            Revoke {revoking.name}? It stops syncing right away. Its local copy stays locked behind your master
            password.
          </p>
        </ConfirmDialog>
      )}
    </div>
  );
}
