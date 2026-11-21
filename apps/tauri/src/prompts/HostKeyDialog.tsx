import { useEffect, useId, useRef, useState } from "react";
import type { HostKeyInfo, Prompt, PromptResponse } from "../api/types";
import { Dialog } from "../ui/Dialog";

type HostKeyPrompt = Extract<Prompt, { kind: "hostKeyUnknown" | "hostKeyChanged" }>;

/** What a changed key's warning requires typing before Accept is enabled. */
export const CONFIRM_WORD = "yes";

/**
 * Accept buttons stay disabled this long after a dialog opens, so a click
 * meant for the previous dialog (a double-click on Accept with two prompts
 * queued) cannot accept a key the user has not seen. Reject is never delayed.
 */
export const ACCEPT_ARM_MS = 400;

export function useArmed(ms: number): boolean {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    const t = setTimeout(() => setArmed(true), ms);
    return () => clearTimeout(t);
  }, [ms]);
  return armed;
}

function KeyFacts({ label, info }: { label: string; info: HostKeyInfo }) {
  return (
    <div className="key-facts">
      <span className="muted small">{label}</span>
      <code className="mono fingerprint">{info.sha256Fingerprint}</code>
      <span className="muted small">{info.algorithm}</span>
    </div>
  );
}

/**
 * Asks whether to trust a server's host key. Never accepts by default:
 * Reject has the focus, and Escape or a click outside rejects.
 *
 * - Unknown host: Accept & save / Accept once / Reject.
 * - Changed key: a blocking warning showing the stored and the presented
 *   fingerprints; accepting (which replaces the stored key) needs `yes`
 *   typed first.
 */
export function HostKeyDialog({
  prompt,
  onRespond,
}: {
  prompt: HostKeyPrompt;
  onRespond: (r: PromptResponse) => void;
}) {
  const rejectRef = useRef<HTMLButtonElement>(null);
  const [typed, setTyped] = useState("");
  const confirmId = useId();
  const armed = useArmed(ACCEPT_ARM_MS);
  const reject = () => onRespond("reject");
  const accept = (save: boolean) => {
    if (armed) onRespond({ accept: { save } });
  };
  const where = prompt.port === 22 ? prompt.host : `${prompt.host}:${prompt.port}`;

  if (prompt.kind === "hostKeyUnknown") {
    return (
      <Dialog title="Unknown host key" onClose={reject} initialFocus={rejectRef}>
        <div className="dialog-content">
          <p>
            The authenticity of <strong>{where}</strong> can't be established. This is the first connection to it,
            or its key was never saved.
          </p>
          <KeyFacts label="Fingerprint" info={prompt.key} />
          <p className="muted small">Compare the fingerprint with one obtained from the server's administrator.</p>
        </div>
        <div className="dialog-actions">
          <button type="button" ref={rejectRef} onClick={reject}>
            Reject
          </button>
          <button type="button" disabled={!armed} onClick={() => accept(false)}>
            Accept once
          </button>
          <button type="button" className="primary" disabled={!armed} onClick={() => accept(true)}>
            Accept & save
          </button>
        </div>
      </Dialog>
    );
  }

  const confirmed = typed.trim().toLowerCase() === CONFIRM_WORD;
  return (
    <Dialog title="Host key changed" onClose={reject} initialFocus={rejectRef} wide>
      <div className="dialog-content">
        <p role="alert" className="warning">
          <strong>Warning:</strong> the host key of <strong>{where}</strong> is not the one saved for it. Someone
          could be intercepting this connection, or the server's key was replaced.
        </p>
        <KeyFacts label="Saved key" info={prompt.old} />
        <KeyFacts label="Presented key" info={prompt.new} />
        <div className="field">
          <label htmlFor={confirmId}>Type {CONFIRM_WORD} to accept the new key</label>
          <input
            id={confirmId}
            autoComplete="off"
            spellCheck={false}
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
          />
        </div>
      </div>
      <div className="dialog-actions">
        <button type="button" className="primary" ref={rejectRef} onClick={reject}>
          Reject
        </button>
        <button
          type="button"
          className="danger"
          disabled={!confirmed || !armed}
          onClick={() => {
            if (confirmed) accept(true);
          }}
        >
          Accept new key
        </button>
      </div>
    </Dialog>
  );
}
