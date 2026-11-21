import { useId, useRef, useState } from "react";
import { api } from "../api";
import type { Snippet } from "../api/types";
import { Dialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { snippetsChanged } from "./useSnippets";

interface SnippetEditorProps {
  /** An existing snippet to edit (with `id`), or the start of a new one (Save as snippet fills `text`). */
  initial?: Partial<Snippet>;
  /** The saved snippet, or null when cancelled. */
  onClose(saved: Snippet | null): void;
}

/** New snippet / Edit snippet: label, command, run after sending. */
export function SnippetEditor({ initial, onClose }: SnippetEditorProps) {
  const editing = initial?.id !== undefined;
  const [label, setLabel] = useState(initial?.label ?? "");
  const [text, setText] = useState(initial?.text ?? "");
  const [run, setRun] = useState(initial?.run ?? false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const labelRef = useRef<HTMLInputElement>(null);
  const labelId = useId();
  const textId = useId();
  const runId = useId();
  const ready = label.trim() !== "" && text.trim() !== "" && !busy;

  async function save() {
    setBusy(true);
    setError(null);
    try {
      const saved = await api.upsertEntity("snippet", { id: initial?.id, label: label.trim(), text, run });
      snippetsChanged();
      onClose(saved);
    } catch (e) {
      setError(describeError(e));
      setBusy(false);
    }
  }

  return (
    <Dialog title={editing ? "Edit snippet" : "New snippet"} onClose={busy ? undefined : () => onClose(null)} initialFocus={labelRef}>
      <form
        className="form"
        onSubmit={(e) => {
          e.preventDefault();
          if (ready) void save();
        }}
      >
        <div className="dialog-content">
          <div className="field">
            <label htmlFor={labelId}>Label</label>
            <input id={labelId} ref={labelRef} value={label} onChange={(e) => setLabel(e.target.value)} />
          </div>
          <div className="field">
            <label htmlFor={textId}>Command</label>
            <textarea
              id={textId}
              className="mono"
              rows={4}
              value={text}
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
              onChange={(e) => setText(e.target.value)}
            />
          </div>
          <div className="field">
            <label className="choice" htmlFor={runId}>
              <input id={runId} type="checkbox" checked={run} onChange={(e) => setRun(e.target.checked)} />
              <span>Run after sending</span>
            </label>
            <p className="muted small">Presses Enter after pasting the command.</p>
          </div>
        </div>
        {error && (
          <p role="alert" className="form-error">
            {error}
          </p>
        )}
        <div className="dialog-actions">
          <button type="button" onClick={() => onClose(null)} disabled={busy}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={!ready}>
            Save
          </button>
        </div>
      </form>
    </Dialog>
  );
}
