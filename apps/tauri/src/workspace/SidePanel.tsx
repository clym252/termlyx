import { useEffect } from "react";
import type { Snippet } from "../api/types";
import { SnippetList } from "../snippets/SnippetList";
import { useSnippetDialogs } from "../snippets/useSnippetDialogs";
import { useSnippets } from "../snippets/useSnippets";
import { getSidePanel, setSidePanel, useSidePanel, type PanelTab } from "./sidePanelPref";
import { ThemesTab } from "./ThemesTab";
import type { PaneSpec } from "./model";

interface SidePanelProps {
  /** The pane snippets go to (the active tab's focused pane); null with none. */
  pane: PaneSpec | null;
  /** Whether that pane has an open session. */
  canSend: boolean;
  send(snippet: Snippet): void;
  /** The phone layout: docked under the terminal in place of the keyboard, one of the two at a time. */
  phone: boolean;
}

const TABS: { id: PanelTab; label: string }[] = [
  { id: "snippets", label: "Snippets" },
  { id: "themes", label: "Themes" },
];

/** Beside the terminals: send snippets, pick a theme. On the phone layout, under the terminal, showing one of the two. */
export function SidePanel({ pane, canSend, send, phone }: SidePanelProps) {
  const { tab } = useSidePanel();
  const close = () => setSidePanel({ open: false, tab });
  const { snippets, error } = useSnippets();
  const d = useSnippetDialogs();

  // Phone: the panel takes the keyboard's place; tapping the terminal (the keyboard back) puts it away.
  useEffect(() => {
    if (!phone) return;
    if (document.activeElement?.closest(".terminal-host")) (document.activeElement as HTMLElement).blur();
    const onFocus = (e: FocusEvent) => {
      if (e.target instanceof Element && e.target.closest(".terminal-host")) setSidePanel({ ...getSidePanel(), open: false });
    };
    document.addEventListener("focusin", onFocus);
    return () => document.removeEventListener("focusin", onFocus);
  }, [phone]);

  const tabs = (
    <div role="group" aria-label="Show" className="side-panel-tabs">
      {TABS.map((t) => (
        <button
          key={t.id}
          type="button"
          aria-pressed={tab === t.id}
          onClick={() => setSidePanel({ open: true, tab: t.id })}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
  const body = (
    <div className="side-panel-body">
      {tab === "snippets" ? (
        <>
          {error && (
            <p role="alert" className="form-error">
              {error}
            </p>
          )}
          {snippets && (
            <SnippetList
              snippets={snippets}
              disabledReason={canSend ? null : "Open a session to send snippets."}
              onPick={send}
              onNew={() => d.edit()}
              onEdit={(s) => d.edit(s)}
              onDelete={d.remove}
            />
          )}
        </>
      ) : (
        <ThemesTab pane={pane} />
      )}
      {d.dialogs}
    </div>
  );

  // The phone opens Snippets and Themes from separate menu items: one thing at a time, no switch.
  if (phone) {
    const title = tab === "snippets" ? "Snippets" : "Themes";
    return (
      <aside className="side-panel side-panel-phone" aria-label={title}>
        <div className="side-panel-head">
          <h2 className="side-panel-title">{title}</h2>
          <button type="button" className="icon-button" aria-label="Close" onClick={close}>
            ×
          </button>
        </div>
        {body}
      </aside>
    );
  }
  return (
    <aside className="side-panel" aria-label="Side panel">
      <div className="side-panel-head">
        {tabs}
        <button type="button" className="icon-button" aria-label="Close panel" onClick={close}>
          ×
        </button>
      </div>
      {body}
    </aside>
  );
}
