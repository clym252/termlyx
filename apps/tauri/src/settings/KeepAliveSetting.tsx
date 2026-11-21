/**
 * Settings → Background (iOS and Android): Keep sessions alive in the
 * background, off by default on iOS and on by default on Android (where a
 * foreground service with a count-only notification does the job, needs no
 * permission and so never turns itself off; the choice is stored as "on" or
 * "off", no entry meaning on). While it is on and a session or port forward is open, the
 * backend (`keepalive.rs`) keeps location updates running so iOS does not
 * suspend Tether; the fixes are dropped unread. Turning it on asks for
 * location permission (When In Use) the first time; only the toggle ever
 * asks. A refusal, a permission withdrawn in iOS Settings or "Allow Once"
 * ending turns it back off with a note.
 *
 * Per device: the choice lives in this webview's localStorage (not a
 * secret, not synced). At start-up (`applyKeepAlive`, from the Gate) it is
 * turned back on only if the permission is still given; from then on the
 * stored choice follows the backend's `keepalive` events (`followKeepAlive`).
 */

import { useEffect, useId, useRef, useState } from "react";
import { api } from "../api";
import type { TetherApi, Unlisten } from "../api/types";
import { IS_ANDROID } from "../platform";
import { describeError } from "../ui/errors";
import { APP_NAME } from "../app/brand";

const STORAGE_KEY = "tether.keepalive";
const ON = "on";
const OFF = "off";

// Android never needs a permission, so the note below only ever shows on iOS.
const DENIED_NOTE = `Location permission was not given, so ${APP_NAME} can't stay running in the background.`;

function storedOn(): boolean {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return IS_ANDROID ? v !== OFF : v === ON;
  } catch {
    return IS_ANDROID;
  }
}

function store(on: boolean) {
  try {
    if (IS_ANDROID) localStorage.setItem(STORAGE_KEY, on ? ON : OFF);
    else if (on) localStorage.setItem(STORAGE_KEY, ON);
    else localStorage.removeItem(STORAGE_KEY);
  } catch {
    // Storage unavailable: the change still applies for this run.
  }
}

/**
 * Start-up (iOS, Android): turns keep-alive back on for a device that chose it, if
 * location permission is still given. Never asks for it (no system prompt at
 * launch, before unlock, out of context): not given (denied, withdrawn,
 * "Allow Once" ended) forgets the choice, so Settings shows it off. A failing
 * backend leaves it off for this run and keeps the choice.
 */
export async function applyKeepAlive(api: Pick<TetherApi, "keepAliveStatus" | "keepAliveSet">): Promise<void> {
  if (!storedOn()) return;
  try {
    if (!(await api.keepAliveStatus()).authorized) {
      store(false);
      return;
    }
    if (!(await api.keepAliveSet(true)).enabled) store(false);
  } catch {
    // Stays off until the next start or a change in Settings.
  }
}

/** Keeps the stored choice in step with the backend (Gate, for the app's life on iOS). */
export function followKeepAlive(api: Pick<TetherApi, "onKeepAlive">): Promise<Unlisten> {
  return api.onKeepAlive((s) => store(s.enabled));
}

export function KeepAliveSetting() {
  const titleId = useId();
  const toggleId = useId();
  // What the backend has in force; `null` until it answers.
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [denied, setDenied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The box's state for the event handler (which outlives renders).
  const shown = useRef<boolean | null>(null);
  shown.current = enabled;

  useEffect(() => {
    let live = true;
    api.keepAliveStatus().then(
      (s) => live && setEnabled(s.enabled),
      (e) => live && setError(describeError(e)),
    );
    // Turned off by the backend (permission withdrawn, "Allow Once" ended): uncheck, say why.
    const unlisten = api.onKeepAlive((s) => {
      if (!live) return;
      if (shown.current === true && !s.enabled && !s.authorized) setDenied(true);
      setEnabled(s.enabled);
      store(s.enabled);
    });
    return () => {
      live = false;
      void unlisten.then((u) => u());
    };
  }, []);

  async function change(next: boolean) {
    setBusy(true);
    setError(null);
    setDenied(false);
    try {
      // On waits for the answer to iOS's location permission prompt.
      const status = await api.keepAliveSet(next);
      setEnabled(status.enabled);
      store(status.enabled);
      setDenied(next && !status.enabled);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="form settings-form" aria-labelledby={titleId} onSubmit={(e) => e.preventDefault()}>
      <h2 id={titleId}>Background</h2>
      <div className="field">
        <label className="choice" htmlFor={toggleId}>
          <input
            id={toggleId}
            type="checkbox"
            checked={enabled === true}
            disabled={enabled === null || busy}
            onChange={(e) => void change(e.target.checked)}
          />
          <span>Keep sessions alive in the background</span>
        </label>
        <p className="muted small">
          {IS_ANDROID
            ? `Shows a notification while a session or port forward is open, so Android keeps ${APP_NAME} running in the background. Uses more battery.`
            : `Uses location in the background so iOS keeps ${APP_NAME} running while a session or port forward is open. Uses more battery. ${APP_NAME} never stores or sends your location.`}
        </p>
        {denied && (
          <p role="alert" className="form-error">
            {DENIED_NOTE}
          </p>
        )}
      </div>
      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}
    </form>
  );
}
