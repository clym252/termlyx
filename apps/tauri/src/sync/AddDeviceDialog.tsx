import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api";
import { isApiError, type LinkCode } from "../api/types";
import { copyText } from "../ui/clipboard";
import { Dialog } from "../ui/Dialog";
import { QrCode } from "./QrCode";
import { SyncProblem } from "./SyncProblem";
import { problemFromCheckError, problemFromError, type Problem } from "./status";

/** "Expires in 10 minutes" from the milliseconds left. */
export function expiryText(msLeft: number): string {
  if (msLeft <= 0) return "This code has expired.";
  if (msLeft <= 60_000) return "Expires in less than a minute";
  const minutes = Math.ceil(msLeft / 60_000);
  return `Expires in ${minutes} ${minutes === 1 ? "minute" : "minutes"}`;
}

/**
 * Settings → Sync → "Add device": makes a single-use add-device code and
 * shows it with its expiry, as text and as a QR code for the iPhone or iPad
 * to scan. The code is held only while the dialog is open; the QR code is
 * drawn from it at render time and kept nowhere.
 */
export function AddDeviceDialog({ onClose }: { onClose: () => void }) {
  const [link, setLink] = useState<LinkCode | null>(null);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [busy, setBusy] = useState(true);
  const [now, setNow] = useState(() => Date.now());
  const [copy, setCopy] = useState<"idle" | "copied" | "failed">("idle");
  // The address other devices reach the server by: edited here, then a new code names it.
  const [editing, setEditing] = useState(false);
  const [address, setAddress] = useState("");

  const load = useCallback(async (at?: string) => {
    setBusy(true);
    setProblem(null);
    setCopy("idle");
    try {
      setLink(await api.sync.linkCode(at));
      setNow(Date.now());
      setEditing(false);
    } catch (e) {
      // A bad address keeps the code that was showing; anything else leaves no code.
      if (at === undefined) setLink(null);
      setProblem(at === undefined ? problemFromError(e) : isApiError(e) && e.code === "invalidAddress" ? problemFromCheckError(e, at) : problemFromError(e));
    } finally {
      setBusy(false);
    }
  }, []);

  // One code per opening, even when development mode runs effects twice.
  const started = useRef(false);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    void load();
  }, [load]);

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);

  const left = link ? link.expiresAt - now : 0;
  const expired = link !== null && left <= 0;

  async function copyCode() {
    if (!link) return;
    setCopy((await copyText(link.code)) ? "copied" : "failed");
  }

  return (
    <Dialog title="Add a device" onClose={onClose}>
      <div className="dialog-content">
        {link && !expired && (
          <>
            <label className="field">
              <span>Add-device code</span>
              <input
                className="mono"
                readOnly
                value={link.code}
                spellCheck={false}
                onFocus={(e) => e.currentTarget.select()}
              />
            </label>
            <QrCode text={link.code} label="QR code for this add-device code" />
            <p>
              Server address in this code: <strong>{link.server}</strong>{" "}
              {!editing && (
                <button type="button" className="link" onClick={() => setEditing(true)}>
                  Edit
                </button>
              )}
            </p>
            {editing && (
              <div className="row">
                <label className="field">
                  <span>Server address</span>
                  <input
                    className="mono"
                    value={address}
                    placeholder="192.168.1.20 or nas.local"
                    autoComplete="off"
                    autoCapitalize="off"
                    spellCheck={false}
                    autoFocus
                    onChange={(e) => setAddress(e.target.value)}
                  />
                </label>
                <button type="button" onClick={() => void load(address.trim())} disabled={busy || !address.trim()}>
                  Apply
                </button>
              </div>
            )}
            <p>
              {expiryText(left)}{" "}
              <span className="muted">(at {new Date(link.expiresAt).toLocaleTimeString()})</span>
            </p>
            <p className="muted small">
              On the new device choose Join a synced vault, then scan this QR code or paste the code. It works once;
              make another for the next device.
            </p>
            <div className="row">
              <button type="button" onClick={() => void copyCode()}>
                Copy code
              </button>
              <span role="status" className="muted">
                {copy === "copied" && "Copied"}
                {copy === "failed" && "Could not copy. Select the code and copy it by hand."}
              </span>
            </div>
          </>
        )}
        {expired && <p role="status">This code has expired.</p>}
        {busy && !link && <p role="status" className="muted">Making a code…</p>}
        {problem && <SyncProblem problem={problem} />}
      </div>
      <div className="dialog-actions">
        {(expired || problem) && (
          <button type="button" onClick={() => void load()} disabled={busy}>
            New code
          </button>
        )}
        <button type="button" className="primary" onClick={onClose}>
          Done
        </button>
      </div>
    </Dialog>
  );
}
