import { useState } from "react";
import { RowMenu } from "../ui/RowMenu";

type PhoneTopBarProps =
  | {
      kind: "page";
      /** The page's name (the page's own heading carries it for screen readers). */
      title: string;
      onMenu(): void;
      /** ＋ at the right: on Hosts only. */
      onNewHost?: () => void;
    }
  | {
      kind: "terminal";
      /** The shown session's label. */
      label: string;
      onBack(): void;
      /** The session menu, in order. */
      actions: [string, () => void][];
    };

/**
 * The phone layout's bar over the page or the terminal: ☰, the page's name
 * and ＋ (Hosts) over a page; ‹ Hosts, the session's label and ⋯ over the
 * terminal.
 */
export function PhoneTopBar(p: PhoneTopBarProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  if (p.kind === "page") {
    return (
      <header className="phone-top" data-kind="page">
        <button type="button" className="phone-top-button" aria-label="Menu" aria-haspopup="dialog" onClick={p.onMenu}>
          ☰
        </button>
        <span className="phone-top-title" aria-hidden="true">
          {p.title}
        </span>
        {p.onNewHost ? (
          <button type="button" className="phone-top-button" aria-label="New host" onClick={p.onNewHost}>
            ＋
          </button>
        ) : (
          <span className="phone-top-spacer" />
        )}
      </header>
    );
  }
  return (
    <header className="phone-top" data-kind="terminal">
      <button type="button" className="phone-top-button phone-back" aria-label="Back to hosts" onClick={p.onBack}>
        ‹ Hosts
      </button>
      <span className="phone-top-title" title={p.label}>
        {p.label}
      </span>
      <span className="phone-top-menu">
        <button
          type="button"
          className="phone-top-button"
          aria-label="Session menu"
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          data-menu-trigger="phone-session"
          onClick={() => setMenuOpen((o) => !o)}
        >
          ⋯
        </button>
        {menuOpen && <RowMenu owner="phone-session" label="Session menu" items={p.actions} onClose={() => setMenuOpen(false)} />}
      </span>
    </header>
  );
}
