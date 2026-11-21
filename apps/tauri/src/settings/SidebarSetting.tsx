import { useId } from "react";
import { useSidebarMode, type SidebarMode } from "../app/sidebarPref";

const CHOICES: [SidebarMode, string][] = [
  ["full", "Full"],
  ["rail", "Icons"],
  ["hidden", "Hidden"],
];

/** Settings → Sidebar: full, icon rail or hidden (desktop and tablet; hidden opens it as the phone's drawer from ☰). Saved on this device. */
export function SidebarSetting() {
  const [mode, setMode] = useSidebarMode();
  const id = useId();
  return (
    <section className="form settings-form" aria-labelledby={id}>
      <h2 id={id}>Sidebar</h2>
      <div role="radiogroup" aria-labelledby={id} className="theme-options sidebar-choices">
        {CHOICES.map(([m, label]) => (
          <button
            key={m}
            type="button"
            role="radio"
            aria-checked={mode === m}
            className="theme-option"
            onClick={() => setMode(m)}
          >
            {label}
          </button>
        ))}
      </div>
    </section>
  );
}
