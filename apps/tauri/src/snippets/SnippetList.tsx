import { useState } from "react";
import type { Snippet } from "../api/types";
import { RowMenu } from "../ui/RowMenu";

interface SnippetListProps {
  snippets: Snippet[];
  /** A row's click: send it (terminal panel) or edit it (Snippets page). */
  onPick(snippet: Snippet): void;
  onNew(): void;
  onEdit(snippet: Snippet): void;
  onDelete(snippet: Snippet): void;
  /** Rows can't be picked, and why (no session to send to). */
  disabledReason?: string | null;
}

/** Search, New snippet and the list: shared by the Snippets page and the terminal's side panel. */
export function SnippetList({ snippets, onPick, onNew, onEdit, onDelete, disabledReason }: SnippetListProps) {
  const [query, setQuery] = useState("");
  const [menu, setMenu] = useState<string | null>(null);
  const q = query.trim().toLowerCase();
  const shown = q ? snippets.filter((s) => s.label.toLowerCase().includes(q) || s.text.toLowerCase().includes(q)) : snippets;

  return (
    <div className="snippet-list">
      <div className="snippet-list-head">
        <input
          type="search"
          aria-label="Search snippets"
          placeholder="Search snippets"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <button type="button" onClick={onNew}>
          New snippet
        </button>
      </div>
      {disabledReason && <p className="muted small">{disabledReason}</p>}
      {snippets.length === 0 ? (
        <p className="muted empty">No snippets yet. Save a command you run often, then send it to any terminal.</p>
      ) : (
        <ul aria-label="Snippets">
          {shown.map((s) => (
            <li key={s.id} className="snippet-row">
              <button
                type="button"
                className="snippet-main"
                disabled={!!disabledReason}
                onClick={() => onPick(s)}
              >
                <span className="snippet-label">{s.label}</span>
                <span className="snippet-command mono">{s.text.split("\n")[0]}</span>
              </button>
              <span className="snippet-menu-wrap">
                <button
                  type="button"
                  className="icon-button"
                  aria-label={`Actions for ${s.label}`}
                  aria-haspopup="menu"
                  aria-expanded={menu === s.id}
                  data-menu-trigger={s.id}
                  onClick={() => setMenu((m) => (m === s.id ? null : s.id))}
                >
                  ⋯
                </button>
                {menu === s.id && (
                  <RowMenu
                    owner={s.id}
                    label={s.label}
                    onClose={() => setMenu(null)}
                    items={[
                      ["Edit", () => onEdit(s)],
                      ["Delete", () => onDelete(s)],
                    ]}
                  />
                )}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
