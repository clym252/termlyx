import { useState, type FormEvent } from "react";
import { api } from "../api";
import type { SshKey } from "../api/types";
import { copyText } from "../ui/clipboard";
import { ConfirmDialog, Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";

interface KeyEditorProps {
  sshKey: SshKey;
  onClose: () => void;
  /** The label was saved. */
  onSaved: () => void;
}

/**
 * A key's details: label (the only editable field), public key,
 * fingerprint. Exporting the private key is an explicit action behind a
 * confirmation: it goes from the backend straight to the clipboard and is
 * not kept anywhere in the UI.
 */
export function KeyEditor({ sshKey, onClose, onSaved }: KeyEditorProps) {
  const [label, setLabel] = useState(sshKey.label);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [exporting, setExporting] = useState(false);

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

  async function exportPrivate() {
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      // The only place a private key reaches the UI: straight into the clipboard.
      const copied = await copyText((await api.keyExportPrivate(sshKey.id)).privateOpenssh);
      setExporting(false);
      if (copied) setNote("Private key copied. It stays on the clipboard until you copy something else.");
      else setError("Could not copy the private key to the clipboard.");
    } catch (err) {
      setExporting(false);
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
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
            <button type="button" className="danger-text" onClick={() => setExporting(true)} disabled={busy}>
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
        <ConfirmDialog
          title="Export private key"
          confirmLabel="Copy private key"
          danger
          busy={busy}
          onConfirm={() => void exportPrivate()}
          onCancel={() => setExporting(false)}
        >
          <p>
            Copy the private key <strong>{sshKey.label}</strong> to the clipboard as plain text? Anyone who
            has it can sign in to every server that trusts this key.
          </p>
        </ConfirmDialog>
      )}
    </>
  );
}
