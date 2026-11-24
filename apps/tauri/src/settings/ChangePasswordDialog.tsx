import { useState, type FormEvent } from "react";
import { api } from "../api";
import { isApiError } from "../api/types";
import { useSyncStatus } from "../sync/store";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { checkNewPassword, field } from "../vault/password";

/**
 * Settings → Security → "Change master password". The three passwords are
 * read from the uncontrolled fields at submit and the form is reset after
 * the attempt, as on the create and unlock screens. With sync on the server
 * hears of it first (so it needs a connection) and the other devices lock
 * and ask for the new password.
 */
export function ChangePasswordDialog({ onClose }: { onClose: () => void }) {
  const { sync } = useSyncStatus();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [changed, setChanged] = useState(false);
  const syncOn = sync?.account != null;

  async function submit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const form = e.currentTarget;
    const data = new FormData(form);
    const current = field(data, "current");
    const next = field(data, "next");
    if (!current) {
      setError("Enter your current master password.");
      return;
    }
    const problem = checkNewPassword(next, field(data, "confirm"));
    if (problem) {
      setError(problem);
      return;
    }
    setError(null);
    setBusy(true);
    try {
      await api.vault.changePassword(current, next);
      form.reset();
      setChanged(true);
    } catch (err) {
      form.reset();
      setError(
        isApiError(err) && err.code === "unreachable"
          ? `${describeError(err)}. Connect to the server to change the password while sync is on.`
          : describeError(err),
      );
    } finally {
      setBusy(false);
    }
  }

  if (changed) {
    return (
      <Dialog title="Change master password" onClose={onClose}>
        <div className="dialog-content">
          <p role="status">Master password changed.</p>
          {syncOn && (
            <p className="muted">
              Your other devices lock and ask for the new password; one that was offline asks the next time it
              reaches the server.
            </p>
          )}
        </div>
        <div className="dialog-actions">
          <button type="button" className="primary" onClick={onClose}>
            Done
          </button>
        </div>
      </Dialog>
    );
  }

  return (
    <Dialog title="Change master password" onClose={busy ? undefined : onClose}>
      <form className="form" onSubmit={(e) => void submit(e)}>
        <div className="dialog-content">
          <label className="field">
            <span>Current master password</span>
            <input name="current" type="password" autoComplete="current-password" autoFocus />
          </label>
          <label className="field">
            <span>New master password</span>
            <input name="next" type="password" autoComplete="new-password" />
          </label>
          <label className="field">
            <span>Confirm new master password</span>
            <input name="confirm" type="password" autoComplete="new-password" />
          </label>
          {syncOn && (
            <p className="muted small">
              Sync is on: this changes the password of your sync account too, so it needs a connection to the server.
            </p>
          )}
          <p className="muted small">
            Copies of your vault made before the change, such as backups, still open with the old password.
          </p>
        </div>
        {error && (
          <p role="alert" className="form-error">
            {error}
          </p>
        )}
        <div className="dialog-actions">
          <button type="button" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={busy}>
            {busy ? "Changing…" : "Change password"}
          </button>
        </div>
      </form>
    </Dialog>
  );
}
