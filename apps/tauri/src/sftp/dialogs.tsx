import { useId, useRef, useState, type FormEvent } from "react";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";

interface NameDialogProps {
  title: string;
  label: string;
  confirmLabel: string;
  initial?: string;
  /** A message when `name` cannot be used, else `null`. */
  validate(name: string): string | null;
  onSubmit(name: string): Promise<void>;
  onClose(): void;
}

/** Asks for one name (a new folder, a new file name). */
export function NameDialog({ title, label, confirmLabel, initial = "", validate, onSubmit, onClose }: NameDialogProps) {
  const fieldId = useId();
  const input = useRef<HTMLInputElement>(null);
  const [name, setName] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const problem = validate(name);
    if (problem) return setError(problem);
    setError(null);
    setBusy(true);
    try {
      await onSubmit(name);
    } catch (err) {
      setError(describeError(err));
      setBusy(false);
    }
  }

  return (
    <Dialog title={title} onClose={busy ? undefined : onClose} initialFocus={input}>
      <form className="form" onSubmit={(e) => void submit(e)}>
        <div className="field">
          <label htmlFor={fieldId}>{label}</label>
          <input
            id={fieldId}
            ref={input}
            value={name}
            autoComplete="off"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            aria-invalid={error ? true : undefined}
            onChange={(e) => setName(e.target.value)}
            onFocus={(e) => e.target.select()}
          />
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
            {confirmLabel}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

const OCTAL = /^[0-7]{3,4}$/;

/** Parses a 3 or 4 digit octal permission string; `null` if it is not one. */
export function parseOctalMode(text: string): number | null {
  const t = text.trim();
  return OCTAL.test(t) ? parseInt(t, 8) : null;
}

interface ChmodDialogProps {
  name: string;
  /** The permission bits now (`mode & 0o7777`). */
  mode: number;
  onSubmit(mode: number): Promise<void>;
  onClose(): void;
}

/** Edits a remote entry's permissions as an octal number. */
export function ChmodDialog({ name, mode, onSubmit, onClose }: ChmodDialogProps) {
  const fieldId = useId();
  const input = useRef<HTMLInputElement>(null);
  const [text, setText] = useState(mode.toString(8).padStart(3, "0"));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const parsed = parseOctalMode(text);
    if (parsed === null) return setError("Enter 3 or 4 octal digits (0 to 7), for example 644 or 0755.");
    setError(null);
    setBusy(true);
    try {
      await onSubmit(parsed);
    } catch (err) {
      setError(describeError(err));
      setBusy(false);
    }
  }

  return (
    <Dialog title="Permissions" onClose={busy ? undefined : onClose} initialFocus={input}>
      <form className="form" onSubmit={(e) => void submit(e)}>
        <p>
          Permissions of <strong className="prewrap">{name}</strong>
        </p>
        <div className="field">
          <label htmlFor={fieldId}>Mode (octal)</label>
          <input
            id={fieldId}
            ref={input}
            className="mono"
            value={text}
            inputMode="numeric"
            autoComplete="off"
            spellCheck={false}
            aria-invalid={error ? true : undefined}
            onChange={(e) => setText(e.target.value)}
            onFocus={(e) => e.target.select()}
          />
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
            Apply
          </button>
        </div>
      </form>
    </Dialog>
  );
}
