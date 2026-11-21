import { useId, useRef, useState } from "react";
import type { ConflictPolicy, Prompt, PromptResponse } from "../api/types";
import { ACCEPT_ARM_MS, useArmed } from "../prompts/HostKeyDialog";
import { Dialog } from "../ui/Dialog";
import { escapeName, formatBytes } from "./paths";
import { transferStore } from "./transfers";

type ConflictPrompt = Extract<Prompt, { kind: "transferConflict" }>;

/**
 * A transfer's destination already exists. The choices are Overwrite, Skip,
 * Rename (write to the first free `name (n)`) and Resume (continue after
 * the bytes already there; only when the existing file is smaller).
 *
 * Nothing is chosen by default: Skip has the focus, Escape or a click
 * outside skips, and the choices that write stay disabled for a moment after
 * the dialog opens, so a click meant for the dialog before it (two
 * conflicts queued) cannot overwrite a file the user has not seen.
 *
 * "Apply to all" remembers the choice for the rest of the batch the job
 * belongs to (see `TransferStore`); those conflicts are then answered
 * without a dialog.
 */
export function ConflictDialog({
  prompt,
  onRespond,
}: {
  prompt: ConflictPrompt;
  onRespond: (r: PromptResponse) => void;
}) {
  const skipRef = useRef<HTMLButtonElement>(null);
  const allId = useId();
  const [applyToAll, setApplyToAll] = useState(false);
  const armed = useArmed(ACCEPT_ARM_MS);
  const canResume = prompt.existingSize < prompt.incomingSize;

  const choose = (policy: ConflictPolicy) => {
    if (applyToAll) transferStore.rememberForBatch(prompt.jobId, policy);
    onRespond({ conflict: policy });
  };

  return (
    // Escape or a click outside skips this file only, whatever the checkbox says.
    <Dialog title="File already exists" onClose={() => onRespond({ conflict: "skip" })} initialFocus={skipRef}>
      <div className="dialog-content">
        <p>
          <code className="mono prewrap path">{escapeName(prompt.path)}</code>
        </p>
        <dl className="facts">
          <dt>Existing file</dt>
          <dd>{formatBytes(prompt.existingSize)}</dd>
          <dt>Incoming file</dt>
          <dd>{formatBytes(prompt.incomingSize)}</dd>
        </dl>
        <p className="muted small">
          Rename keeps both files. Resume continues the existing file from where it ends
          {canResume ? "." : ", which needs it to be smaller than the incoming one."}
        </p>
        <div className="row">
          <input id={allId} type="checkbox" checked={applyToAll} onChange={(e) => setApplyToAll(e.target.checked)} />
          <label htmlFor={allId}>Apply to all remaining conflicts in this batch</label>
        </div>
      </div>
      <div className="dialog-actions">
        <button type="button" ref={skipRef} onClick={() => choose("skip")}>
          Skip
        </button>
        <button type="button" disabled={!armed} onClick={() => choose("rename")}>
          Rename
        </button>
        <button type="button" disabled={!armed || !canResume} onClick={() => choose("resume")}>
          Resume
        </button>
        <button type="button" className="danger" disabled={!armed} onClick={() => choose("overwrite")}>
          Overwrite
        </button>
      </div>
    </Dialog>
  );
}
