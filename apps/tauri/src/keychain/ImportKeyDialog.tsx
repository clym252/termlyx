import { useRef, useState, type ChangeEvent, type FormEvent } from "react";
import { api } from "../api";
import { isApiError } from "../api/types";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";

interface ImportKeyDialogProps {
  onClose: () => void;
  /** A key was imported; the dialog is done. */
  onImported: () => void;
}

/**
 * Imports a private key pasted as text or read from a file. The key text
 * and the passphrase live in the form's own inputs (read at submit, never
 * in React state) and are cleared when the dialog ends. An encrypted key
 * is answered with `needsPassphrase`: the dialog then asks for the
 * passphrase and retries with the same text.
 */
export function ImportKeyDialog({ onClose, onImported }: ImportKeyDialogProps) {
  const text = useRef<HTMLTextAreaElement>(null);
  const passphrase = useRef<HTMLInputElement>(null);
  const [label, setLabel] = useState("");
  const [needsPassphrase, setNeedsPassphrase] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  function clearSecrets() {
    if (text.current) text.current.value = "";
    if (passphrase.current) passphrase.current.value = "";
  }

  function close() {
    clearSecrets();
    onClose();
  }

  async function readFile(e: ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    try {
      const content = await file.text();
      if (text.current) text.current.value = content;
      setLabel((current) => current || file.name);
      setNeedsPassphrase(false);
      setNotice(null);
      setError(null);
    } catch {
      setError(`Could not read ${file.name}.`);
    }
  }

  async function submit(e: FormEvent) {
    e.preventDefault();
    const key = text.current?.value ?? "";
    if (!key.trim()) {
      setError("Paste a private key or choose a key file.");
      return;
    }
    const phrase = passphrase.current?.value ?? "";
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await api.keyImport(key, phrase || undefined, label.trim() || undefined);
      clearSecrets();
      onImported();
    } catch (err) {
      if (passphrase.current) passphrase.current.value = "";
      if (isApiError(err) && err.code === "needsPassphrase") {
        setNeedsPassphrase(true);
        setNotice("This key is encrypted. Enter its passphrase to import it.");
      } else if (isApiError(err) && err.code === "badPassphrase") {
        setNeedsPassphrase(true);
        setError("Wrong passphrase. Try again.");
      } else {
        setError(describeError(err));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog title="Import key" onClose={busy ? undefined : close} wide>
      <form className="form" onSubmit={(e) => void submit(e)}>
        <div className="dialog-content">
          <label className="field">
            <span>Private key</span>
            <textarea
              ref={text}
              className="mono"
              rows={7}
              spellCheck={false}
              autoCapitalize="off"
              autoComplete="off"
              placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
              autoFocus
            />
          </label>
          <label className="field">
            <span>Key file</span>
            <input type="file" onChange={(e) => void readFile(e)} />
          </label>
          <label className="field">
            <span>Label (optional)</span>
            <input value={label} onChange={(e) => setLabel(e.target.value)} />
          </label>
          {needsPassphrase && (
            <label className="field">
              <span>Key passphrase</span>
              <input ref={passphrase} type="password" autoComplete="off" autoFocus />
            </label>
          )}
          {notice && <p role="status">{notice}</p>}
        </div>
        {error && (
          <p role="alert" className="form-error">
            {error}
          </p>
        )}
        <div className="dialog-actions">
          <button type="button" onClick={close} disabled={busy}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={busy}>
            Import
          </button>
        </div>
      </form>
    </Dialog>
  );
}
