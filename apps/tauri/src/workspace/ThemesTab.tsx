import { useId } from "react";
import { updateTerminalSettings, useTerminalSettings } from "../settings/store";
import { ipHighlightOf, themeGroups, type Theme } from "../themes";
import type { PaneSpec } from "./model";
import { useHostLook } from "./TerminalPane";

/** A small terminal drawn in the theme's own colours: a login line (its IP highlighted), a prompt, coloured `ls` output. */
function Swatch({ t }: { t: Theme }) {
  const c = t.terminal;
  return (
    <span className="theme-swatch mono" aria-hidden="true" style={{ background: c.background, color: c.foreground }}>
      <span>
        Last login from <span style={{ color: ipHighlightOf(t), textDecoration: "underline" }}>10.0.0.12</span>
      </span>
      <span>
        <span style={{ color: c.green }}>user@server</span>:<span style={{ color: c.blue }}>~</span>$ ls
      </span>
      <span>
        <span style={{ color: c.blue }}>docs</span> <span style={{ color: c.green }}>backup.sh</span> notes.txt
      </span>
    </span>
  );
}

/** The side panel's Themes tab: every theme as a swatch, applied on click; IP highlighting. */
export function ThemesTab({ pane }: { pane: PaneSpec | null }) {
  const settings = useTerminalSettings();
  const hostTheme = useHostLook(pane).theme;
  const groups = themeGroups();
  const ipsId = useId();

  return (
    <div className="themes-tab">
      {hostTheme && <p className="muted small">This terminal uses {hostTheme.name} from its host settings.</p>}
      {(["dark", "light"] as const).map((g) => (
        <section key={g} className="theme-group">
          <h3 id={`${ipsId}-${g}`}>{g === "dark" ? "Dark" : "Light"}</h3>
          <div role="radiogroup" aria-labelledby={`${ipsId}-${g}`} className="theme-options">
            {groups[g].map((t) => (
              <button
                key={t.id}
                type="button"
                role="radio"
                aria-checked={settings.themeId === t.id}
                aria-label={t.name}
                className="theme-option"
                onClick={() => updateTerminalSettings({ themeId: t.id })}
              >
                <span className="theme-name">{t.name}</span>
                <Swatch t={t} />
              </button>
            ))}
          </div>
        </section>
      ))}
      <label className="choice" htmlFor={ipsId}>
        <input
          id={ipsId}
          type="checkbox"
          checked={settings.highlightIps}
          onChange={(e) => updateTerminalSettings({ highlightIps: e.target.checked })}
        />
        <span>Highlight IP addresses</span>
      </label>
    </div>
  );
}
