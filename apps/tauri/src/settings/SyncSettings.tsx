import { useId, useState } from "react";
import { api } from "../api";
import { AddDeviceDialog } from "../sync/AddDeviceDialog";
import { DevicesList } from "../sync/DevicesList";
import { EnableSyncDialog } from "../sync/EnableSyncDialog";
import { SignInDialog } from "../sync/SignInDialog";
import { SyncProblem } from "../sync/SyncProblem";
import { describeSync, needsSignIn, problemFromError, problemFromState } from "../sync/status";
import { useSyncStatus } from "../sync/store";
import { ConfirmDialog } from "../ui/Dialog";
import { APP_NAME } from "../app/brand";

/**
 * Settings → Sync. Off: "Connect to server". On: the server, the account, the
 * status with the reason when something is wrong, "Sync now", "Add device",
 * the devices list, and "Turn off sync on this device" (keeps the local
 * vault, forgets the sign-in). "Sign in again" appears when the session is
 * gone and sync waits for the master password. Sync problems never get in the way of the
 * local vault.
 */
export function SyncSettings() {
  const titleId = useId();
  const { sync, error } = useSyncStatus();
  const [dialog, setDialog] = useState<"enable" | "addDevice" | "turnOff" | "signIn" | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  async function syncNow() {
    setProblem(null);
    try {
      await api.sync.now();
    } catch (e) {
      setProblem(problemFromError(e).message);
    }
  }

  async function turnOff() {
    setBusy(true);
    setProblem(null);
    try {
      await api.sync.turnOff();
      setDialog(null);
    } catch (e) {
      setProblem(problemFromError(e).message);
    } finally {
      setBusy(false);
    }
  }

  let body;
  if (error) {
    body = (
      <p role="alert" className="form-error">
        {error}
      </p>
    );
  } else if (!sync) {
    body = (
      <p role="status" className="muted">
        Loading…
      </p>
    );
  } else if (!sync.account || sync.status.state === "off") {
    body = (
      <>
        <p className="muted small">
          Keep this vault in step across your devices through your own {APP_NAME} server. Everything is encrypted before
          it leaves this device.
        </p>
        <div className="row">
          <button type="button" onClick={() => setDialog("enable")}>
            Connect to server
          </button>
        </div>
      </>
    );
  } else {
    const view = describeSync(sync.status);
    const issue = problemFromState(sync.status);
    const state = sync.status;
    const refreshKey = state.state === "idle" ? `idle:${state.lastSync}` : state.state;
    body = (
      <>
        <dl className="sync-facts">
          <dt className="muted small">Server</dt>
          <dd className="mono">{sync.account.server}</dd>
          <dt className="muted small">Account</dt>
          <dd>{sync.account.username}</dd>
          <dt className="muted small">Status</dt>
          <dd data-testid="sync-state" data-state={view.tone}>
            <span className={`sync-dot sync-dot-${view.tone}`} aria-hidden="true" /> {view.label}
            {view.detail && <span className="muted"> · {view.detail}</span>}
          </dd>
        </dl>
        {issue && state.state === "error" && state.kind !== "partial" && <SyncProblem problem={issue} />}
        <div className="row">
          {needsSignIn(state) && (
            <button type="button" className="primary" onClick={() => setDialog("signIn")}>
              Sign in again
            </button>
          )}
          <button
            type="button"
            onClick={() => void syncNow()}
            disabled={state.state === "syncing" || (state.state === "error" && state.kind === "revoked")}
          >
            Sync now
          </button>
          <button type="button" onClick={() => setDialog("addDevice")}>
            Add device
          </button>
          <button type="button" className="danger-text" onClick={() => setDialog("turnOff")}>
            Turn off sync on this device
          </button>
        </div>
        <DevicesList refreshKey={refreshKey} />
      </>
    );
  }

  return (
    <section className="form settings-form sync-settings" aria-labelledby={titleId}>
      <h2 id={titleId}>Sync</h2>
      {body}
      {problem && dialog === null && (
        <p role="alert" className="form-error">
          {problem}
        </p>
      )}
      {dialog === "enable" && <EnableSyncDialog onClose={() => setDialog(null)} />}
      {dialog === "addDevice" && <AddDeviceDialog onClose={() => setDialog(null)} />}
      {dialog === "signIn" && <SignInDialog onClose={() => setDialog(null)} />}
      {dialog === "turnOff" && (
        <ConfirmDialog
          title="Turn off sync"
          confirmLabel="Turn off"
          danger
          busy={busy}
          error={problem}
          onConfirm={() => void turnOff()}
          onCancel={() => {
            setProblem(null);
            setDialog(null);
          }}
        >
          <p>
            This device stops syncing and forgets its sign-in. Your local vault stays as it is, and your other devices
            keep syncing.
          </p>
          <p className="muted small">
            This device stays in the device list until you revoke it from another device.
          </p>
        </ConfirmDialog>
      )}
    </section>
  );
}
