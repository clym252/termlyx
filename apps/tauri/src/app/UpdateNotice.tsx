import { useEffect, useState, useSyncExternalStore } from "react";
import { api } from "../api";
import type { UpdateInfo } from "../api/types";
import { getAutoCheck } from "../settings/updatePref";
import { APP_NAME } from "./brand";
import { ConfirmDialog, Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";

/**
 * Self-update, desktop only. One check after the first unlock of a run
 * (`getAutoCheck()`), or Settings → About's "Check for updates". A failed
 * check is only ever shown in About; nothing installs without "Update now",
 * which first asks when sessions, transfers or forwards would end.
 */
export type UpdateStatus =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "upToDate" }
  | { kind: "failed" }
  | { kind: "available"; info: UpdateInfo }
  | { kind: "installing"; info: UpdateInfo; pct: number | null }
  | { kind: "installFailed"; info: UpdateInfo };

interface State {
  status: UpdateStatus;
  /** "Later": the notice stays hidden for this run. */
  dismissed: boolean;
  /** What "Update now" would end, waiting for the user's answer. */
  confirming: string[] | null;
}

let state: State = { status: { kind: "idle" }, dismissed: false, confirming: null };
let autoChecked = false;
const listeners = new Set<() => void>();

function set(next: Partial<State>) {
  state = { ...state, ...next };
  listeners.forEach((l) => l());
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => void listeners.delete(l);
}

export function useUpdateState(): State {
  return useSyncExternalStore(subscribe, () => state);
}

export async function checkForUpdates() {
  set({ status: { kind: "checking" } });
  try {
    const info = await api.updates.check();
    set({ status: info ? { kind: "available", info } : { kind: "upToDate" } });
  } catch {
    // Offline, GitHub down, rate-limited: About says so; nothing else does.
    set({ status: { kind: "failed" } });
  }
}

function plural(n: number, one: string, many: string) {
  return n === 1 ? `1 ${one}` : `${n} ${many}`;
}

/** "Update now": asks first when the restart would end live work, else installs. */
export async function updateNow() {
  let lines: string[];
  try {
    const work = await api.activeWork();
    lines = [
      work.sessions.length && `${plural(work.sessions.length, "open session", "open sessions")} will close.`,
      work.transfers.length && `${plural(work.transfers.length, "transfer", "transfers")} will stop.`,
      work.forwards.length && `${plural(work.forwards.length, "port forward", "port forwards")} will stop.`,
    ].filter((l): l is string => !!l);
  } catch {
    lines = ["Open sessions, transfers and port forwards will end."];
  }
  if (lines.length > 0) set({ confirming: lines });
  else void install();
}

async function install() {
  const { status } = state;
  if (status.kind !== "available" && status.kind !== "installFailed") return;
  const { info } = status;
  set({ confirming: null, status: { kind: "installing", info, pct: null } });
  try {
    // Relaunches on success.
    await api.updates.install((pct) => set({ status: { kind: "installing", info, pct } }));
  } catch (e) {
    // The detail stays out of the UI; a download URL's query (signed tokens) is cut from the log too.
    console.debug("update install failed", describeError(e).replace(/\?[^\s)]*/g, ""));
    set({ status: { kind: "installFailed", info } });
  }
}

/** The non-blocking notice (and the restart confirmation), mounted with the unlocked layout. */
export function UpdateNotice() {
  const { status, dismissed, confirming } = useUpdateState();
  const [showNotes, setShowNotes] = useState(false);

  useEffect(() => {
    if (autoChecked || !api.updates.supported || !getAutoCheck()) return;
    autoChecked = true;
    void checkForUpdates();
  }, []);

  const shown =
    (status.kind === "available" && !dismissed) || status.kind === "installing" || status.kind === "installFailed";
  return (
    <>
      {shown && (
        <section className="notice update-notice" aria-label="Update available">
          <p role="status">Version {status.info.version} is available</p>
          {status.kind === "installing" ? (
            <p role="status" className="muted small">
              {status.pct === null ? "Downloading…" : `Downloading… ${status.pct}%`}
            </p>
          ) : (
            <>
              {status.kind === "installFailed" && (
                <p className="form-error small">Couldn't install the update.</p>
              )}
              <div className="row">
                <button type="button" onClick={() => setShowNotes(true)}>
                  View changes
                </button>
                <button type="button" className="primary" onClick={() => void updateNow()}>
                  Update now
                </button>
                <button type="button" onClick={() => set({ dismissed: true })}>
                  Later
                </button>
              </div>
            </>
          )}
        </section>
      )}
      {showNotes && "info" in status && (
        <Dialog title={`What's new in ${status.info.version}`} onClose={() => setShowNotes(false)}>
          <p className="update-notes">{status.info.notes || "No release notes."}</p>
          <div className="dialog-actions">
            <button type="button" className="primary" onClick={() => setShowNotes(false)}>
              Close
            </button>
          </div>
        </Dialog>
      )}
      {confirming && (
        <ConfirmDialog
          title="Install update"
          confirmLabel="Update and restart"
          onConfirm={() => void install()}
          onCancel={() => set({ confirming: null })}
        >
          <p>{APP_NAME} restarts to finish the update.</p>
          {confirming.map((l) => (
            <p key={l}>{l}</p>
          ))}
        </ConfirmDialog>
      )}
    </>
  );
}
