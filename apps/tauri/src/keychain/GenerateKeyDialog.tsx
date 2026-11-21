import { useId, useState, type FormEvent } from "react";
import { api } from "../api";
import type { KeyAlgo, SshKey } from "../api/types";
import { copyText } from "../ui/clipboard";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";

const ALGORITHMS: { value: KeyAlgo; label: string }[] = [
  { value: "ed25519", label: "Ed25519 (recommended)" },
  { value: "ecdsaP256", label: "ECDSA P-256" },
  { value: "ecdsaP384", label: "ECDSA P-384" },
  { value: "ecdsaP521", label: "ECDSA P-521" },
  { value: "rsa2048", label: "RSA 2048" },
  { value: "rsa4096", label: "RSA 4096" },
];

function isKeyAlgo(v: string): v is KeyAlgo {
  return ALGORITHMS.some((a) => a.value === v);
}

interface GenerateKeyDialogProps {
  onClose: () => void;
  /** A key was created (the dialog stays open to show it). */
  onGenerated: () => void;
}

/**
 * Generates a key pair in the backend. Only the public half comes back: the
 * dialog then shows the fingerprint and offers the public key to copy. The
 * optional passphrase is read from the form at submit and the form is
 * reset; it is never held in React state.
 */
export function GenerateKeyDialog({ onClose, onGenerated }: GenerateKeyDialogProps) {
  const algoId = useId();
  const [algo, setAlgo] = useState<KeyAlgo>("ed25519");
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<SshKey | null>(null);
  const [note, setNote] = useState("Key generated");

  async function submit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const form = e.currentTarget;
    if (!label.trim()) {
      setError("Enter a label for the key.");
      return;
    }
    const passphrase = String(new FormData(form).get("passphrase") ?? "");
    form.reset();
    setError(null);
    setBusy(true);
    try {
      const key = await api.keyGenerate(algo, label.trim(), passphrase || undefined);
      setCreated(key);
      onGenerated();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function copyPublic(key: SshKey) {
    setNote((await copyText(key.publicOpenssh)) ? "Public key copied" : "Could not copy. Use Edit key to select it.");
  }

  if (created) {
    return (
      <Dialog title="Generate key" onClose={onClose}>
        <div className="dialog-content">
          <p role="status">{note}</p>
          <dl className="key-facts">
            <dt className="muted small">Label</dt>
            <dd>{created.label}</dd>
            <dt className="muted small">Type</dt>
            <dd>{created.algorithm}</dd>
            <dt className="muted small">Fingerprint</dt>
            <dd className="mono fingerprint" data-testid="fingerprint">
              {created.fingerprint}
            </dd>
          </dl>
          <p className="muted small">Add the public key to a server's authorized_keys to sign in with this key.</p>
        </div>
        <div className="dialog-actions">
          <button type="button" onClick={() => void copyPublic(created)}>
            Copy public key
          </button>
          <button type="button" className="primary" onClick={onClose}>
            Done
          </button>
        </div>
      </Dialog>
    );
  }

  return (
    <Dialog title="Generate key" onClose={busy ? undefined : onClose}>
      <form className="form" onSubmit={(e) => void submit(e)}>
        <div className="dialog-content">
          <div className="field">
            <label htmlFor={algoId}>Algorithm</label>
            <select
              id={algoId}
              value={algo}
              onChange={(e) => {
                if (isKeyAlgo(e.target.value)) setAlgo(e.target.value);
              }}
            >
              {ALGORITHMS.map((a) => (
                <option key={a.value} value={a.value}>
                  {a.label}
                </option>
              ))}
            </select>
          </div>
          <label className="field">
            <span>Label</span>
            <input value={label} onChange={(e) => setLabel(e.target.value)} autoFocus />
          </label>
          <label className="field">
            <span>Passphrase (optional)</span>
            <input name="passphrase" type="password" autoComplete="new-password" />
          </label>
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
            {busy ? "Generating…" : "Generate"}
          </button>
        </div>
      </form>
    </Dialog>
  );
}
