import { useState, type FormEvent } from "react";
import { api } from "../api";
import { describeError } from "../ui/errors";
import { checkNewPassword, field, MIN_PASSWORD_LENGTH } from "./password";
import { JoinVault } from "../sync/JoinVault";
import { RecoveryKeyDialog } from "./RecoveryKeyDialog";
import { Wordmark } from "../app/Wordmark";

/**
 * First run: choose a master password. The vault is created, then the
 * recovery key is shown once; `onCreated` runs only after "I saved it". "Join a
 * synced vault" starts from an add-device code instead.
 */
export function CreateVault({ onCreated }: { onCreated: () => void }) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Held only while its dialog is open (the key has to be rendered somewhere).
  const [recoveryKey, setRecoveryKey] = useState<string | null>(null);
  // "Join a synced vault" replaces this screen until the user backs out or has joined.
  const [joining, setJoining] = useState(false);

  async function submit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const form = e.currentTarget;
    const data = new FormData(form);
    const password = field(data, "password");
    const problem = checkNewPassword(password, field(data, "confirm"));
    if (problem) {
      setError(problem);
      return;
    }
    setError(null);
    setBusy(true);
    try {
      const created = await api.vaultCreate(password);
      form.reset();
      setRecoveryKey(created.recoveryKey);
    } catch (err) {
      form.reset();
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  if (joining) return <JoinVault onJoined={onCreated} onBack={() => setJoining(false)} />;

  return (
    <main className="center-screen">
      <form className="card form" onSubmit={(e) => void submit(e)} aria-labelledby="create-title">
        <Wordmark size={40} />
        <h1 id="create-title">Create your vault</h1>
        <p className="muted">
          Your hosts, keys and settings are encrypted with a master password that only you know.
        </p>
        <label className="field">
          <span>Master password</span>
          <input
            name="password"
            type="password"
            autoComplete="new-password"
            autoFocus
            required
            aria-describedby="create-hint"
          />
        </label>
        <label className="field">
          <span>Confirm master password</span>
          <input name="confirm" type="password" autoComplete="new-password" required />
        </label>
        <p id="create-hint" className="muted small">
          At least {MIN_PASSWORD_LENGTH} characters.
        </p>
        {error && (
          <p role="alert" className="form-error">
            {error}
          </p>
        )}
        <button type="submit" className="primary" disabled={busy}>
          {busy ? "Creating…" : "Create vault"}
        </button>
        <button type="button" className="link" onClick={() => setJoining(true)} disabled={busy}>
          Join a synced vault
        </button>
      </form>
      {recoveryKey !== null && (
        <RecoveryKeyDialog
          recoveryKey={recoveryKey}
          onSaved={() => {
            setRecoveryKey(null);
            onCreated();
          }}
        />
      )}
    </main>
  );
}
