import { useEffect, useId, useRef, type FormEvent } from "react";
import type { Prompt, PromptResponse } from "../api/types";
import { Dialog } from "../ui/Dialog";
import { SaveToVault } from "./SaveToVault";

type KbdPrompt = Extract<Prompt, { kind: "keyboardInteractive" }>;

/**
 * A keyboard-interactive challenge: one field per server prompt, shown as
 * typed when the server says it may echo and hidden otherwise. Fields are
 * uncontrolled and read only at submit (then cleared), so answers are
 * never in React state. Cancel, Escape or a click outside answers reject.
 * A single hidden prompt with `saveTo` (a password) offers "Save password
 * to vault" (answered as `saveAnswers`).
 */
export function KbdInteractiveDialog({ prompt, onRespond }: { prompt: KbdPrompt; onRespond: (r: PromptResponse) => void }) {
  const inputs = useRef<(HTMLInputElement | null)[]>([]);
  const firstInput = useRef<HTMLInputElement | null>(null);
  const baseId = useId();
  const save = useRef<HTMLInputElement>(null);
  const single = prompt.prompts.length === 1 ? prompt.prompts[0] : null;
  const saveTo = single && !single[1] ? prompt.saveTo : null;

  const clear = () => inputs.current.forEach((el) => el && (el.value = ""));
  // However the dialog goes away (answered, cancelled, withdrawn), typed
  // answers do not stay in the detached fields.
  useEffect(() => {
    const fields = [...inputs.current]; // the elements, not the ref slots (refs are detached first)
    return () => fields.forEach((el) => el && (el.value = ""));
  }, []);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const answers = prompt.prompts.map((_, i) => inputs.current[i]?.value ?? "");
    clear();
    onRespond(saveTo && save.current?.checked ? { saveAnswers: answers } : { answers });
  };
  const cancel = () => {
    clear();
    onRespond("reject");
  };

  return (
    <Dialog title={prompt.name.trim() || "Authentication"} onClose={cancel} initialFocus={firstInput}>
      <form className="form" onSubmit={submit}>
        {prompt.instructions.trim() && <p className="prewrap">{prompt.instructions}</p>}
        {prompt.prompts.map(([text, echo], i) => (
          <div className="field" key={i}>
            <label htmlFor={`${baseId}-${i}`}>{text.trim() || `Response ${i + 1}`}</label>
            <input
              id={`${baseId}-${i}`}
              ref={(el) => {
                inputs.current[i] = el;
                if (i === 0) firstInput.current = el;
              }}
              type={echo ? "text" : "password"}
              autoComplete="off"
              spellCheck={false}
            />
          </div>
        ))}
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
