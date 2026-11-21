import { dracula } from "./palettes/dracula";
import { tokyoNight, tokyoNightStorm, tokyoNightDay } from "./palettes/tokyoNight";
import { catppuccinMocha, catppuccinMacchiato, catppuccinLatte } from "./palettes/catppuccin";
import { rosePine, rosePineMoon } from "./palettes/rosePine";
import { kanagawaWave, kanagawaDragon } from "./palettes/kanagawa";
import { flexokiDark, flexokiLight } from "./palettes/flexoki";
import { gruvboxDark, gruvboxLight } from "./palettes/gruvbox";
import { everforestDark, everforestLight } from "./palettes/everforest";
import { githubDark, githubLight } from "./palettes/github";
import { ayuDark, ayuLight } from "./palettes/ayu";
import { oneLight } from "./palettes/oneLight";
import { nightOwl } from "./palettes/nightOwl";
import { tetherHighContrastDark, tetherHighContrastLight } from "./palettes/tetherHighContrast";
import { monoGreen, monoAmber } from "./palettes/mono";
import { nord } from "./palettes/nord";
import { tetherDark } from "./palettes/tetherDark";
import { tetherLight } from "./palettes/tetherLight";
import type { Theme, ThemeId, UiColors, UiExtras } from "./types";

export type { TerminalColors, Theme, ThemeId, UiColors, UiExtras } from "./types";

/** Every theme, in the order the settings list them. Sources: themes/README.md. */
export const THEMES: readonly Theme[] = [
  tetherDark,
  tetherLight,
  dracula,
  nord,
  tokyoNight,
  tokyoNightStorm,
  tokyoNightDay,
  catppuccinMocha,
  catppuccinMacchiato,
  catppuccinLatte,
  rosePine,
  rosePineMoon,
  kanagawaWave,
  kanagawaDragon,
  flexokiDark,
  flexokiLight,
  gruvboxDark,
  gruvboxLight,
  everforestDark,
  everforestLight,
  githubDark,
  githubLight,
  ayuDark,
  ayuLight,
  oneLight,
  nightOwl,
  tetherHighContrastDark,
  tetherHighContrastLight,
  monoGreen,
  monoAmber,
];

/** The themes as the picker groups them: dark and light; Termlyx Dark/Light, Termlyx's others, then by name. */
export function themeGroups(): { dark: Theme[]; light: Theme[] } {
  const rank = (t: Theme) => (t.id === "tether-dark" || t.id === "tether-light" ? 0 : t.id.startsWith("tether") ? 1 : 2);
  const ordered = [...THEMES].sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
  return { dark: ordered.filter((t) => t.dark), light: ordered.filter((t) => !t.dark) };
}

export const DEFAULT_THEME_ID: ThemeId = "tether-dark";

/** Broadcast amber and host badges (each pair >= 4.5:1) on dark themes. */
const DARK_EXTRAS: UiExtras = {
  broadcast: "#ffb020",
  "on-danger": "#2a0a0a",
  warning: "#e3b341",
  "on-warning": "#1b1500",
  success: "#4ec07a",
  "badge-1-bg": "#23406b",
  "badge-1-fg": "#b4d3ff",
  "badge-2-bg": "#3d2d69",
  "badge-2-fg": "#d3c2ff",
  "badge-3-bg": "#1f4f38",
  "badge-3-fg": "#a6e8c0",
  "badge-4-bg": "#5c4513",
  "badge-4-fg": "#ffdc94",
  "badge-5-bg": "#63283a",
  "badge-5-fg": "#ffbfd0",
  "badge-6-bg": "#175052",
  "badge-6-fg": "#98ebe7",
  "badge-7-bg": "#434f18",
  "badge-7-fg": "#d9f08f",
  "badge-8-bg": "#64361f",
  "badge-8-fg": "#ffc8a6",
};

/** The same hues as light tints with dark text, for light themes. */
const LIGHT_EXTRAS: UiExtras = {
  broadcast: "#a35f00",
  "on-danger": "#ffffff",
  warning: "#8a5a00",
  "on-warning": "#ffffff",
  success: "#1e7a44",
  "badge-1-bg": "#dbe8ff",
  "badge-1-fg": "#1d4f99",
  "badge-2-bg": "#e8e0ff",
  "badge-2-fg": "#4b2f99",
  "badge-3-bg": "#d9f2e3",
  "badge-3-fg": "#1d6b3f",
  "badge-4-bg": "#fdedc8",
  "badge-4-fg": "#6e4a00",
  "badge-5-bg": "#fde0e8",
  "badge-5-fg": "#9a2448",
  "badge-6-bg": "#d3f2f0",
  "badge-6-fg": "#11615e",
  "badge-7-bg": "#ecf3c9",
  "badge-7-fg": "#4d610e",
  "badge-8-bg": "#fee3d3",
  "badge-8-fg": "#94410f",
};

/** All of a theme's app colours: its own, over the dark or light extras. */
export function uiColorsOf(t: Theme): UiColors & UiExtras {
  return { ...(t.dark ? DARK_EXTRAS : LIGHT_EXTRAS), ...t.ui };
}

/** The colour IP addresses are drawn in. */
export function ipHighlightOf(t: Theme): string {
  return t.terminal.ipHighlight ?? (t.dark ? t.terminal.brightMagenta : t.terminal.magenta);
}

export function isThemeId(value: unknown): value is ThemeId {
  return THEMES.some((t) => t.id === value);
}

/** Display names from before the rename to Termlyx (saved in host settings). */
const LEGACY_NAMES: Record<string, ThemeId> = {
  "tether dark": "tether-dark",
  "tether light": "tether-light",
  "tether high contrast dark": "tether-high-contrast-dark",
  "tether high contrast light": "tether-high-contrast-light",
};

/** The theme with this id or name (case-insensitive: host settings store names); the default otherwise. */
export function findTheme(idOrName: string | null | undefined): Theme | undefined {
  if (!idOrName) return undefined;
  const key = idOrName.trim().toLowerCase();
  const legacy = LEGACY_NAMES[key];
  if (legacy) return THEMES.find((t) => t.id === legacy);
  return THEMES.find((t) => t.id === key || t.name.toLowerCase() === key);
}

export function getTheme(id: ThemeId): Theme {
  return THEMES.find((t) => t.id === id) ?? tetherDark;
}

/** Applies a theme's app colours to the document (the CSS variables of ui.css). */
export function applyUiTheme(theme: Theme, root: HTMLElement = document.documentElement) {
  for (const [name, value] of Object.entries(uiColorsOf(theme))) root.style.setProperty(`--${name}`, value);
  root.style.setProperty("color-scheme", theme.dark ? "dark" : "light");
  root.dataset.theme = theme.id;
}
