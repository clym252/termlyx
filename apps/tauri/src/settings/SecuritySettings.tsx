import { useEffect, useId, useState } from "react";
import { api } from "../api";
import type { QuickUnlockKind } from "../api/types";
import { DEVICE_NOUN, FORM_FACTOR, IS_IOS, IS_WINDOWS } from "../platform";
import { describeError } from "../ui/errors";
import {
  AUTO_LOCK_CHOICES,
  applyAutoLock,
  autoLockLabel,
  autoLockValue,
  noteAutoLockApplied,
  parseAutoLock,
  setAutoLockPref,
  useAutoLockError,
  useAutoLockPref,
} from "./autoLockPref";
import { ChangePasswordDialog } from "./ChangePasswordDialog";
import { APP_NAME } from "../app/brand";

/** The quick unlock toggle's label and help for the device's check. */
function quickUnlockWords(kind: QuickUnlockKind): { label: string; help: string } {
  // Android: the system prompt picks fingerprint or face itself.
  if (kind === "androidBiometric") {
    return {
      label: "Biometric unlock",
      help: `Unlock with a fingerprint or face this ${DEVICE_NOUN} rates as strong. Changing enrolled fingerprints or faces turns it off.`,
    };
  }
  if (IS_IOS) {
    // Face ID or Touch ID by device; the backend's gate always asks one of them.
    const device = FORM_FACTOR === "tablet" ? "iPad" : "iPhone";
    const check = kind === "faceId" ? "Face ID" : kind === "touchId" ? "Touch ID" : "Face ID or Touch ID";
    const enrolled =
      kind === "faceId"
        ? " Changing enrolled faces turns it off."
        : kind === "touchId"
          ? " Changing enrolled fingers turns it off."
          : "";
    return {
      label: `Unlock with ${check}`,
      help: `Keeps a key on this ${device} that ${APP_NAME} only uses after ${check}.${enrolled} The master password still works.`,
    };
  }
  if (kind === "touchId") {
    return {
      label: "Unlock with Touch ID",
      help: `Keeps a key in this Mac's keychain that ${APP_NAME} only uses after Touch ID. Adding or removing a fingerprint turns it off. The master password still works.`,
    };
  }
  if (kind === "windowsHello") {
    return {
      label: "Unlock with Windows Hello",
      help: "Keeps a key on this PC that only Windows Hello (your PIN, fingerprint or face) can open. Resetting Windows Hello turns it off. The master password still works.",
    };
  }
  return IS_WINDOWS
    ? {
        label: "Unlock with Credential Manager",
        help: "Keeps a key in this PC's Credential Manager so you can unlock without typing the master password. The master password still works.",
      }
    : {
        label: "Unlock with system keychain",
        help: "Keeps a key in this computer's keychain so you can unlock without typing the master password. There is no fingerprint or password check: on Linux, any program running as you can use the key while your keyring is unlocked. The master password still works.",
      };
}

/**
 * Settings → Security: unlock with Touch ID (macOS), Face ID or Touch ID
 * (iOS), Windows Hello, biometrics (Android) or the system keychain instead of typing the master
 * password. Turning it on stores a vault unlock secret in the OS keychain
 * (after one Touch ID, Face ID or Windows Hello check where there is one),
 * which can refuse (no Touch ID, no Hello PIN, access denied): the reason is
 * shown and the setting stays as it was.
 *
 * Auto-lock: per device; the backend has to take the new limit before it is
 * saved, so what the select shows is what is in force.
 */
export function SecuritySettings() {
  const toggleId = useId();
  const autoLockId = useId();
  const autoLock = useAutoLockPref();
  // The backend did not take the device's choice at start-up (it runs with Never).
  const notApplied = useAutoLockError();
  const [savingAutoLock, setSavingAutoLock] = useState(false);
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [kind, setKind] = useState<QuickUnlockKind>("none");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [changing, setChanging] = useState(false);
  const words = quickUnlockWords(kind);

  useEffect(() => {
    let live = true;
    api.vaultStatus().then(
      (s) => {
        if (!live) return;
        setEnabled(s.quickUnlockEnabled);
        setKind(s.quickUnlockKind);
      },
      (e) => live && setError(describeError(e)),
    );
    return () => {
      live = false;
    };
  }, []);

  async function change(next: boolean) {
    setBusy(true);
    setError(null);
    try {
      if (next) await api.vaultEnableQuickUnlock();
      else await api.vaultDisableQuickUnlock();
      setEnabled(next);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  async function changeAutoLock(raw: string) {
    const next = parseAutoLock(raw);
    if (next === undefined) return;
    setSavingAutoLock(true);
    setError(null);
    try {
      await api.setAutoLock(next);
      setAutoLockPref(next);
      noteAutoLockApplied(null);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setSavingAutoLock(false);
    }
  }

  async function retryAutoLock() {
    setSavingAutoLock(true);
    await applyAutoLock((m) => api.setAutoLock(m));
    setSavingAutoLock(false);
  }

  return (
    <>
      <form className="form settings-form" aria-labelledby="security-settings-title" onSubmit={(e) => e.preventDefault()}>
        <h2 id="security-settings-title">Security</h2>
        <div className="field">
          <label className="choice" htmlFor={toggleId}>
            <input
              id={toggleId}
              type="checkbox"
              checked={enabled === true}
              disabled={enabled === null || busy}
              onChange={(e) => void change(e.target.checked)}
            />
            <span>{words.label}</span>
          </label>
          <p className="muted small">{words.help}</p>
        </div>
        <div className="field">
          <label htmlFor={autoLockId}>Auto-lock</label>
          <select
            id={autoLockId}
            value={autoLockValue(autoLock)}
            disabled={savingAutoLock}
            onChange={(e) => void changeAutoLock(e.target.value)}
          >
            {AUTO_LOCK_CHOICES.map((m) => (
              <option key={autoLockValue(m)} value={autoLockValue(m)}>
                {autoLockLabel(m)}
              </option>
            ))}
          </select>
          <p className="muted small">
            Locks the vault once {APP_NAME} has been in the background this long. Time spent in {APP_NAME} never counts.
            Open sessions keep running behind the unlock screen.
          </p>
          {notApplied !== null && (
            <div role="alert" className="form-error">
              <p>Auto-lock is not on: {notApplied}. {APP_NAME} will not lock by itself until it is.</p>
              <button type="button" disabled={savingAutoLock} onClick={() => void retryAutoLock()}>
                Try again
              </button>
            </div>
          )}
        </div>
        <div className="field">
          <div className="row">
            <button type="button" onClick={() => setChanging(true)}>
              Change master password
            </button>
          </div>
          <p className="muted small">Re-protects your vault with a new master password.</p>
        </div>
        {error && (
          <p role="alert" className="form-error">
            {error}
          </p>
        )}
      </form>
      {changing && <ChangePasswordDialog onClose={() => setChanging(false)} />}
    </>
  );
}
