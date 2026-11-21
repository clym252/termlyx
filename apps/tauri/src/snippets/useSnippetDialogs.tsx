import { useState, type ReactNode } from "react";
import { api } from "../api";
import type { Snippet } from "../api/types";
import { ConfirmDialog } from "../ui/Dialog";
import { describeError } from "../ui/errors";
import { SnippetEditor } from "./SnippetEditor";
import { snippetsChanged } from "./useSnippets";

type Open = { kind: "edit"; initial?: Partial<Snippet> } | { kind: "delete"; snippet: Snippet } | null;

/** The editor and the delete confirmation, for any view that lists snippets (every list reloads after a change). */
export function useSnippetDialogs(): {
  edit(initial?: Partial<Snippet>): void;
  remove(snippet: Snippet): void;
  dialogs: ReactNode;
} {
  const [open, setOpen] = useState<Open>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function confirmDelete(s: Snippet) {
    setBusy(true);
    setError(null);
    try {
      await api.deleteEntity("snippet", s.id);
      setOpen(null);
      snippetsChanged();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  const dialogs =
    open?.kind === "edit" ? (
      <SnippetEditor
        initial={open.initial}
        onClose={() => setOpen(null)}
      />
    ) : open?.kind === "delete" ? (
      <ConfirmDialog
        title="Delete snippet?"
        confirmLabel="Delete"
        danger
        busy={busy}
        error={error}
        onCancel={() => setOpen(null)}
        onConfirm={() => void confirmDelete(open.snippet)}
      >
        <p>
          <strong>{open.snippet.label}</strong> will be removed from every device.
        </p>
      </ConfirmDialog>
    ) : null;

  return {
    edit: (initial) => setOpen({ kind: "edit", initial }),
    remove: (snippet) => {
      setError(null);
      setOpen({ kind: "delete", snippet });
    },
    dialogs,
  };
}
