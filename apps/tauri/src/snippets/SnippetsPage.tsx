import { SnippetList } from "./SnippetList";
import { useSnippetDialogs } from "./useSnippetDialogs";
import { useSnippets } from "./useSnippets";

/** Snippets (sidebar): manage the saved commands; the terminal's side panel sends them. */
export function SnippetsPage() {
  const { snippets, error } = useSnippets();
  const d = useSnippetDialogs();

  return (
    <section className="page snippets-page" aria-labelledby="snippets-title">
      <header className="page-head">
        <h1 id="snippets-title">Snippets</h1>
      </header>
      {error && (
        <p role="alert" className="form-error">
          {error}
        </p>
      )}
      {snippets && (
        <SnippetList
          snippets={snippets}
          onPick={(s) => d.edit(s)}
          onNew={() => d.edit()}
          onEdit={(s) => d.edit(s)}
          onDelete={d.remove}
        />
      )}
      {d.dialogs}
    </section>
  );
}
