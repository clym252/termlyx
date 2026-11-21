import { useEffect, useId, useState } from "react";
import { api } from "../api";
import type { LocalShell } from "../api/types";
import { IS_MOBILE } from "../platform";
import { FontSelect } from "../ui/FontSelect";
import { THEMES, isThemeId } from "../themes";
import { FONT_SIZE_MAX, FONT_SIZE_MIN, updateTerminalSettings, useTerminalSettings } from "./store";

/** Settings → Terminal: theme (app and terminals), IP highlighting, font family and size, local shell (not on iOS). Saved on this device. */
export function TerminalSettings() {
  const settings = useTerminalSettings();
  const themeId = useId();
  const familyId = useId();
  const sizeId = useId();
  const shellId = useId();
  const ipsId = useId();
  const [shells, setShells] = useState<LocalShell[]>([]);
  useEffect(() => {
    // There is no local terminal on iOS: nothing to list, nothing to pick.
    if (IS_MOBILE) return;
    let live = true;
    api
      .localShells()
      .then((list) => live && setShells(list))
      .catch(() => live && setShells([]));
    return () => {
      live = false;
    };
  }, []);
  // The size input keeps what is typed; only valid values are applied.
  const [size, setSize] = useState(String(settings.fontSize));

  return (
    <form
      className="form settings-form"
      aria-labelledby="terminal-settings-title"
      onSubmit={(e) => e.preventDefault()}
    >
      <h2 id="terminal-settings-title">Terminal</h2>
      <div className="field">
        <label htmlFor={themeId}>Theme</label>
        <select
          id={themeId}
          value={settings.themeId}
          onChange={(e) => {
            if (isThemeId(e.target.value)) updateTerminalSettings({ themeId: e.target.value });
          }}
        >
          {THEMES.map((t) => (
            <option key={t.id} value={t.id}>
              {t.name}
            </option>
          ))}
        </select>
        <p className="muted small">
          Applies to the app and to every terminal; a host's colour scheme setting wins.
        </p>
      </div>
      <div className="field">
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
      <div className="field">
        <label htmlFor={familyId}>Font family</label>
        <FontSelect
          id={familyId}
          value={settings.fontFamily}
          onChange={(fontFamily) => updateTerminalSettings({ fontFamily })}
        />
      </div>
      <div className="field">
        <label htmlFor={sizeId}>Font size</label>
        <input
          id={sizeId}
          type="number"
          min={FONT_SIZE_MIN}
          max={FONT_SIZE_MAX}
          step={1}
          onWheel={(e) => e.currentTarget.blur()}
          value={size}
          onChange={(e) => {
            setSize(e.target.value);
            const n = Number(e.target.value);
            if (Number.isInteger(n)) updateTerminalSettings({ fontSize: n });
          }}
        />
      </div>
      {shells.length > 1 && (
        <div className="field">
          <label htmlFor={shellId}>Local shell</label>
          <select
            id={shellId}
            value={settings.localShellId ?? shells[0].id}
            onChange={(e) => updateTerminalSettings({ localShellId: e.target.value === shells[0].id ? null : e.target.value })}
          >
            {shells.map((sh, i) => (
              <option key={sh.id} value={sh.id}>
                {i === 0 ? `${sh.label} (default)` : sh.label}
              </option>
            ))}
            {settings.localShellId !== null && !shells.some((sh) => sh.id === settings.localShellId) && (
              <option value={settings.localShellId}>{settings.localShellId} (not available)</option>
            )}
          </select>
          <p className="muted small">What new local terminals run. Open tabs keep their shell.</p>
        </div>
      )}
      <p className="muted small">Saved on this device.</p>
    </form>
  );
}
