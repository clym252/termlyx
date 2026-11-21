import { useEffect, useRef, useState, type FormEvent } from "react";
import { api } from "../api";
import { isApiError, type QuickUnlockKind } from "../api/types";
import { IS_IOS } from "../platform";
import { describeError } from "../ui/errors";
import { checkNewPassword, field } from "./password";
import { APP_NAME } from "../app/brand";
import { Wordmark } from "../app/Wordmark";

export const PASSWORD_CHANGED_MESSAGE = "Master password was changed on another device. Enter the new one.";

export const WINDOWS_HELLO_CHANGED_MESSAGE =
  "Windows Hello changed, so quick unlock was turned off. Unlock with your master password, then turn it on again in Settings → Security.";

/** Whether this window has already asked Touch ID, Face ID or Windows Hello by itself (once per load). */
let autoPrompted = false;

/** Lets tests stand in for a fresh window load. */
export function resetAutoPromptForTests() {
  autoPrompted = false;
}

/**
 * Unlock with the master password, or reset it with the recovery key. With
 * quick unlock enabled, a button unlocks with the system keychain instead;
 * if that fails the password form stays, with the reason shown.
 *
 * With Touch ID (macOS, iOS), Face ID (iOS) or Windows Hello the window's
 * first unlock screen asks by itself; a lock by hand does not. Declining is not an error: the password
 * field takes over, as it does quietly when the automatic attempt finds the
 * check unavailable (no sensor, lid closed, no Hello PIN set up, a remote
 * session). Changed fingers or faces, or a reset Hello credential, turn quick
 * unlock off, so the button goes away.
 *
 * `passwordChanged`: sync locked the vault because another device changed
 * the master password; the screen says so above the password field.
 */
export function UnlockScreen({
  onUnlocked,
  quickUnlock = false,
  quickUnlockKind = "none",
  passwordChanged = false,
}: {
  onUnlocked: () => void;
  quickUnlock?: boolean;
  quickUnlockKind?: QuickUnlockKind;
  /** The vault locked because another device changed the master password. */
  passwordChanged?: boolean;
}) {
  const [mode, setMode] = useState<"password" | "recovery">("password");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [quickOffered, setQuickOffered] = useState(quickUnlock);
  const passwordField = useRef<HTMLInputElement>(null);
  const touchId = quickUnlockKind === "touchId";
  const faceId = quickUnlockKind === "faceId";
  const windowsHello = quickUnlockKind === "windowsHello";
  const android = quickUnlockKind === "androidBiometric";
  const biometric = touchId || faceId || windowsHello || android;

  async function run(form: HTMLFormElement, action: () => Promise<void>) {
    setError(null);
    setBusy(true);
    try {
      await action();
      form.reset();
      onUnlocked();
    } catch (err) {
      form.reset();
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  function unlock(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const form = e.currentTarget;
    const password = field(new FormData(form), "password");
    void run(form, () => api.vaultUnlock(password));
  }

  function recover(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const form = e.currentTarget;
    const data = new FormData(form);
    const newPassword = field(data, "newPassword");
    const problem = checkNewPassword(newPassword, field(data, "confirm"));
    if (problem) {
      setError(problem);
      return;
    }
    const recoveryKey = field(data, "recoveryKey").trim();
    void run(form, () => api.vaultUnlockRecovery(recoveryKey, newPassword));
  }

  /** `auto`: the screen's own first attempt, which keeps "quick unlock is not available" quiet. */
  async function unlockQuickly(auto = false) {
    setError(null);
    setBusy(true);
    try {
      await api.vaultQuickUnlock();
      onUnlocked();
    } catch (err) {
      if (isApiError(err) && err.code === "biometryChanged") setQuickOffered(false);
      const quiet =
        isApiError(err) && (err.code === "presenceDenied" || (auto && err.code === "biometryUnavailable"));
      if (!quiet) {
        const hello = windowsHello && isApiError(err) && err.code === "biometryChanged";
        setError(hello ? WINDOWS_HELLO_CHANGED_MESSAGE : describeError(err));
      }
      setBusy(false);
      passwordField.current?.focus();
      return;
    }
    setBusy(false);
  }

  useEffect(() => {
    if (!biometric || !quickUnlock || autoPrompted) return;
    autoPrompted = true;
    void unlockQuickly(true);
    // Once per window load, on the first unlock screen.
  }, []);

  function switchTo(next: "password" | "recovery") {
    setError(null);
    setMode(next);
  }

  return (
    <main className="center-screen">
      {mode === "password" ? (
        <form className="card form" onSubmit={unlock} aria-labelledby="unlock-title">
          <Wordmark size={40} />
          <h1 id="unlock-title">Unlock {APP_NAME}</h1>
          {passwordChanged && (
            <p role="status" className="notice">
              {PASSWORD_CHANGED_MESSAGE}
            </p>
          )}
          {quickOffered && (
            <button type="button" onClick={() => void unlockQuickly()} disabled={busy}>
              {touchId
                ? "Unlock with Touch ID"
                : faceId
                  ? "Unlock with Face ID"
                  : windowsHello
                    ? "Unlock with Windows Hello"
                    : android
                      ? "Unlock with biometrics"
                      : IS_IOS
                        ? "Unlock with Face ID or Touch ID"
                        : "Quick unlock"}
            </button>
          )}
          <label className="field">
            <span>Master password</span>
            <input
              ref={passwordField}
              name="password"
              type="password"
              autoComplete="current-password"
              autoFocus
              required
            />
          </label>
          {error && (
            <p role="alert" className="form-error">
              {error}
            </p>
          )}
          <button type="submit" className="primary" disabled={busy}>
            {busy ? "Unlocking…" : "Unlock"}
          </button>
          <button type="button" className="link" onClick={() => switchTo("recovery")}>
            Use recovery key
          </button>
        </form>
      ) : (
        <form className="card form" onSubmit={recover} aria-labelledby="recover-title">
          <h1 id="recover-title">Use recovery key</h1>
          <p className="muted">Enter your recovery key and choose a new master password.</p>
          <label className="field">
            <span>Recovery key</span>
            <input
              className="mono"
              name="recoveryKey"
              autoComplete="off"
              autoCapitalize="off"
              spellCheck={false}
              autoFocus
              required
            />
          </label>
          <label className="field">
            <span>New master password</span>
            <input name="newPassword" type="password" autoComplete="new-password" required />
          </label>
          <label className="field">
            <span>Confirm new master password</span>
            <input name="confirm" type="password" autoComplete="new-password" required />
          </label>
          {error && (
            <p role="alert" className="form-error">
              {error}
            </p>
          )}
          <button type="submit" className="primary" disabled={busy}>
            {busy ? "Unlocking…" : "Reset password and unlock"}
          </button>
          <button type="button" className="link" onClick={() => switchTo("password")}>
            Back to password
          </button>
        </form>
      )}
    </main>
  );
}
