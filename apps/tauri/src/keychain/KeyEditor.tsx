import { useState, type FormEvent } from "react";
import { api } from "../api";
import type { SshKey } from "../api/types";
import { IS_MOBILE } from "../platform";
import { clearClipboardLater, copyText } from "../ui/clipboard";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { field } from "../vault/password";

interface KeyEditorProps {
  sshKey: SshKey;
  onClose: () => void;
  /** The label was saved. */
  onSaved: () => void;
}

/**
 * A key's details: label (the only editable field), public key,
 * fingerprint. Exporting the private key asks for the master password
 * again. On the desktop the backend saves it through the native save dialog
 * and it never reaches the page; on a phone or tablet it goes straight to
 * the clipboard, which is cleared after a minute if it still holds the key.
 * The key and the password are never kept in the UI.
 */
export function KeyEditor({ sshKey, onClose, onSaved }: KeyEditorProps) {
  const [label, setLabel] = useState(sshKey.label);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);

  async function save(e: FormEvent) {
    e.preventDefault();
    if (!label.trim()) {
      setError("A key needs a label.");
      return;
    }
    setError(null);
    setBusy(true);
    try {
      await api.upsertEntity("ssh_key", { id: sshKey.id, label: label.trim() });
      onSaved();
    } catch (err) {
      setError(describeError(err));
      setBusy(false);
    }
  }

  async function copyPublic() {
    setNote((await copyText(sshKey.publicOpenssh)) ? "Public key copied" : "Could not copy. Select the key and copy it by hand.");
  }

  async function exportPrivate(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const form = e.currentTarget;
    const password = field(new FormData(form), "password");
    if (!password) {
      setExportError("Enter your master password.");
      return;
    }
    setBusy(true);
    setExportError(null);
    setNote(null);
    try {
      if (IS_MOBILE) {
        // The only place a private key reaches the UI: straight into the clipboard.
        const text = (await api.keyExportPrivate(sshKey.id, password)).privateOpenssh;
        if (!(await api.writeClipboard(text))) throw new Error("Could not copy the private key to the clipboard.");
        clearClipboardLater(text);
        setNote("Private key copied. The clipboard is cleared in a minute if it still holds the key.");
      } else {
        const path = await api.keySavePrivate(sshKey.id, password);
        // Cancelled in the save dialog: stay here.
        if (path === null) {
          form.reset();
          return;
        }
        setNote(`Private key saved to ${path}`);
      }
      setExporting(false);
    } catch (err) {
      form.reset();
      setExportError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  function startExport() {
    setExportError(null);
    setExporting(true);
  }

  return (
    <>
      <Dialog title={`Edit key ${sshKey.label}`} onClose={busy ? undefined : onClose} wide>
        <form className="form" onSubmit={(e) => void save(e)}>
          <div className="dialog-content">
            <label className="field">
              <span>Label</span>
              <input value={label} onChange={(e) => setLabel(e.target.value)} autoFocus />
            </label>
            <label className="field">
              <span>Public key</span>
              <textarea
                className="mono"
                rows={3}
                readOnly
                spellCheck={false}
                value={sshKey.publicOpenssh}
                onFocus={(e) => e.currentTarget.select()}
              />
            </label>
            <dl className="key-facts">
              <dt className="muted small">Fingerprint</dt>
              <dd className="mono fingerprint">{sshKey.fingerprint}</dd>
              <dt className="muted small">Protection</dt>
              <dd>{sshKey.hasPassphrase ? "Protected by a passphrase" : "No passphrase"}</dd>
            </dl>
            {note && <p role="status">{note}</p>}
          </div>
          {error && (
            <p role="alert" className="form-error">
              {error}
            </p>
          )}
          <div className="dialog-actions">
            <button type="button" onClick={() => void copyPublic()} disabled={busy}>
              Copy public key
            </button>
            <button type="button" className="danger-text" onClick={startExport} disabled={busy}>
              Export private key…
            </button>
            <button type="button" onClick={onClose} disabled={busy}>
              Close
            </button>
            <button type="submit" className="primary" disabled={busy || label.trim() === sshKey.label}>
              Save
            </button>
          </div>
        </form>
      </Dialog>
      {exporting && (
        <Dialog title="Export private key" onClose={busy ? undefined : () => setExporting(false)}>
          <form className="form" onSubmit={(e) => void exportPrivate(e)}>
            <div className="dialog-content">
              <p>
                {IS_MOBILE ? "Copy" : "Save"} the private key <strong>{sshKey.label}</strong> as plain text? Anyone
                who has it can sign in to every server that trusts this key.
              </p>
              <label className="field">
                <span>Master password</span>
                <input name="password" type="password" autoComplete="current-password" autoFocus />
              </label>
            </div>
            {exportError && (
              <p role="alert" className="form-error">
                {exportError}
              </p>
            )}
            <div className="dialog-actions">
              <button type="button" onClick={() => setExporting(false)} disabled={busy}>
                Cancel
              </button>
              <button type="submit" className="danger" disabled={busy}>
                {IS_MOBILE ? "Copy private key" : "Save private key…"}
              </button>
            </div>
          </form>
        </Dialog>
      )}
    </>
  );
}
