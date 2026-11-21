import { useEffect, useId, useRef, type FormEvent } from "react";
import type { Prompt, PromptResponse } from "../api/types";
import { Dialog } from "../ui/Dialog";
import { SaveToVault } from "./SaveToVault";

type SecretPrompt = Extract<Prompt, { kind: "passphrase" | "password" }>;

/**
 * Asks for a key passphrase or a password. The field is uncontrolled: the
 * value is read once at submit, sent, and cleared; it is never in React
 * state. Cancel, Escape or a click outside answers reject. A password
 * prompt with `saveTo` offers "Save password to vault" (answered as
 * `saveSecret`).
 */
export function SecretDialog({ prompt, onRespond }: { prompt: SecretPrompt; onRespond: (r: PromptResponse) => void }) {
  const input = useRef<HTMLInputElement>(null);
  const save = useRef<HTMLInputElement>(null);
  const fieldId = useId();
  const isPassphrase = prompt.kind === "passphrase";
  const saveTo = prompt.kind === "password" ? prompt.saveTo : null;

  // However the dialog goes away (answered, cancelled, withdrawn), the
  // typed secret does not stay in the detached field.
  useEffect(() => {
    const el = input.current;
    return () => {
      if (el) el.value = "";
    };
  }, []);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const el = input.current;
    if (!el) return;
    const secret = el.value;
    el.value = "";
    onRespond(saveTo && save.current?.checked ? { saveSecret: secret } : { secret });
  };
  const cancel = () => {
    if (input.current) input.current.value = "";
    onRespond("reject");
  };

  return (
    <Dialog title={isPassphrase ? "Key passphrase" : "Password"} onClose={cancel} initialFocus={input}>
      <form className="form" onSubmit={submit}>
        <p>
          {isPassphrase ? (
            <>
              Enter the passphrase for <strong>{prompt.keyLabel}</strong>.
            </>
          ) : (
            <>
              Password for{" "}
              <strong>
                {prompt.user}@{prompt.host}
              </strong>
            </>
          )}
        </p>
        <div className="field">
          <label htmlFor={fieldId}>{isPassphrase ? "Passphrase" : "Password"}</label>
          <input id={fieldId} ref={input} type="password" autoComplete="off" spellCheck={false} />
        </div>
        {saveTo && <SaveToVault target={saveTo} checkbox={save} />}
        <div className="dialog-actions">
          <button type="button" onClick={cancel}>
            Cancel
          </button>
          <button type="submit" className="primary">
            Continue
          </button>
        </div>
      </form>
    </Dialog>
  );
}
