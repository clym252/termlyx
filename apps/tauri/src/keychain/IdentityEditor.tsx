import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { api } from "../api";
import type { Identity, IdentityInput, SshKey } from "../api/types";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";

type PasswordChoice = "keep" | "clear" | "set";

interface IdentityEditorProps {
  /** The identity being edited, or `null` for a new one. */
  identity: Identity | null;
  keys: SshKey[];
  onClose: () => void;
  onSaved: () => void;
}

/**
 * Label, username, optional password and the key an identity signs in
 * with. The password is read from its input at submit and never kept in
 * React state; the saved identity only reports whether it has one. For an
 * existing password the choice is keep, remove or replace
 * (`IdentityInput.password`: omitted, `null`, a string).
 */
export function IdentityEditor({ identity, keys, onClose, onSaved }: IdentityEditorProps) {
  const keyId = useId();
  const password = useRef<HTMLInputElement>(null);
  const [label, setLabel] = useState(identity?.label ?? "");
  const [username, setUsername] = useState(identity?.username ?? "");
  const [key, setKey] = useState<string>(identity?.keyId ?? "");
  // As found when the editor opened: the radio choices below do not change under the user.
  const [hadPassword] = useState(identity?.hasPassword ?? false);
  const [choice, setChoice] = useState<PasswordChoice>(hadPassword ? "keep" : "set");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The chosen key was deleted while the editor was open: no key (saving a gone one fails).
  useEffect(() => {
    if (key !== "" && !keys.some((k) => k.id === key)) setKey("");
  }, [keys, key]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!label.trim() || !username.trim()) {
      setError("An identity needs a label and a username.");
      return;
    }
    const input: IdentityInput = {
      ...(identity ? { id: identity.id } : {}),
      label: label.trim(),
      username: username.trim(),
      keyId: key || null,
    };
    if (choice === "clear") {
      input.password = null;
    } else if (choice === "set") {
      const typed = password.current?.value ?? "";
      if (typed) input.password = typed;
      else if (hadPassword) {
        setError("Enter the new password, or keep the current one.");
        return;
      }
    }
    setError(null);
    setBusy(true);
    try {
      await api.upsertEntity("identity", input);
      if (password.current) password.current.value = "";
      onSaved();
    } catch (err) {
      setError(describeError(err));
      setBusy(false);
    }
  }

  const close = () => {
    if (password.current) password.current.value = "";
    onClose();
  };

  return (
    <Dialog
      title={identity ? `Edit identity ${identity.label}` : "New identity"}
      onClose={busy ? undefined : close}
      wide
    >
      <form className="form" onSubmit={(e) => void submit(e)}>
        <div className="dialog-content">
          <label className="field">
            <span>Label</span>
            <input value={label} onChange={(e) => setLabel(e.target.value)} autoFocus={!identity} />
          </label>
          <label className="field">
            <span>Username</span>
            <input
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              spellCheck={false}
              autoCapitalize="off"
            />
          </label>

          {hadPassword ? (
            <fieldset className="choices">
              <legend>Password</legend>
              <label className="choice">
                <input type="radio" name="password" checked={choice === "keep"} onChange={() => setChoice("keep")} />
                <span>Keep the current password</span>
              </label>
              <label className="choice">
                <input type="radio" name="password" checked={choice === "clear"} onChange={() => setChoice("clear")} />
                <span>Remove the password</span>
              </label>
              <label className="choice">
                <input type="radio" name="password" checked={choice === "set"} onChange={() => setChoice("set")} />
                <span>Set a new password</span>
              </label>
            </fieldset>
          ) : null}
          {choice === "set" && (
            <label className="field">
              <span>{hadPassword ? "New password" : "Password (optional)"}</span>
              <input ref={password} type="password" autoComplete="new-password" />
            </label>
          )}

          <div className="field">
            <label htmlFor={keyId}>Key</label>
            <select id={keyId} value={key} onChange={(e) => setKey(e.target.value)}>
              <option value="">None</option>
              {keys.map((k) => (
                <option key={k.id} value={k.id}>
                  {k.label}
                </option>
              ))}
            </select>
          </div>
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
            Save identity
          </button>
        </div>
      </form>
    </Dialog>
  );
}
