import { useState, type FormEvent } from "react";
import { api } from "../api";
import { field } from "../vault/password";
import { Dialog } from "../ui/Dialog";
import { SyncProblem } from "./SyncProblem";
import { problemFromError, type Problem } from "./status";

/**
 * "Sign in again": sync stopped because this device's session is gone (the
 * server was restored from an older backup, or the sign-in ended). The master
 * password signs the same device back in; nothing in the vault changes. The
 * password field is uncontrolled and cleared after every attempt.
 */
export function SignInDialog({ onClose }: { onClose: () => void }) {
  const [problem, setProblem] = useState<Problem | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const form = e.currentTarget;
    const password = field(new FormData(form), "password");
    if (!password) {
      setProblem({ message: "Enter your master password.", hint: null, severe: false });
      return;
    }
    setProblem(null);
    setBusy(true);
    try {
      await api.sync.signIn(password);
      form.reset();
      onClose();
    } catch (err) {
      form.reset();
      setProblem(problemFromError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog title="Sign in again" onClose={busy ? undefined : onClose}>
      <form className="form" onSubmit={(e) => void submit(e)}>
        <div className="dialog-content">
          <p>
            This device's sign-in to the sync server has ended, so sync has stopped. Enter your master password to
            sign in again. Your vault stays as it is.
          </p>
          <label className="field">
            <span>Master password</span>
            <input name="password" type="password" autoComplete="current-password" autoFocus />
          </label>
        </div>
        {problem && <SyncProblem problem={problem} />}
        <div className="dialog-actions">
          <button type="button" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={busy}>
            {busy ? "Signing in…" : "Sign in"}
          </button>
        </div>
      </form>
    </Dialog>
  );
}
