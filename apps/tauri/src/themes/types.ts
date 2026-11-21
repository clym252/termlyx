/** Colours of the terminal itself (xterm's `ITheme`). */
export interface TerminalColors {
  background: string;
  foreground: string;
  cursor: string;
  cursorAccent: string;
  selectionBackground: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
  /** IP addresses in output; default: bright magenta on a dark theme, magenta on a light one. */
  ipHighlight?: string;
}

/** The app's colour variables (ui.css), without the leading `--`. */
export interface UiColors {
  bg: string;
  panel: string;
  "panel-raised": string;
  border: string;
  text: string;
  muted: string;
  accent: string;
  "accent-text": string;
  danger: string;
  "danger-bg": string;
  focus: string;
}

/**
 * The rest of the app's colour variables. Each theme gets the dark or light
 * set (`themes/index.ts`) unless it sets its own in `Theme.ui`.
 */
export interface UiExtras {
  /** Typing goes to several panes. */
  broadcast: string;
  /** Text on a `danger` background. */
  "on-danger": string;
  /** Needs attention (conflicts, offline sync); readable as text on panels. */
  warning: string;
  /** Text on a `warning` background. */
  "on-warning": string;
  /** All is well (synced). */
  success: string;
  "badge-1-bg": string;
  "badge-1-fg": string;
  "badge-2-bg": string;
  "badge-2-fg": string;
  "badge-3-bg": string;
  "badge-3-fg": string;
  "badge-4-bg": string;
  "badge-4-fg": string;
  "badge-5-bg": string;
  "badge-5-fg": string;
  "badge-6-bg": string;
  "badge-6-fg": string;
  "badge-7-bg": string;
  "badge-7-fg": string;
  "badge-8-bg": string;
  "badge-8-fg": string;
}

export type ThemeId =
  | "tether-dark"
  | "tether-light"
  | "dracula"
  | "nord"
  | "tokyo-night"
  | "tokyo-night-storm"
  | "tokyo-night-day"
  | "catppuccin-mocha"
  | "catppuccin-macchiato"
  | "catppuccin-latte"
  | "rose-pine"
  | "rose-pine-moon"
  | "kanagawa-wave"
  | "kanagawa-dragon"
  | "flexoki-dark"
  | "flexoki-light"
  | "gruvbox-dark"
  | "gruvbox-light"
  | "everforest-dark"
  | "everforest-light"
  | "github-dark"
  | "github-light"
  | "ayu-dark"
  | "ayu-light"
  | "one-light"
  | "night-owl"
  | "tether-high-contrast-dark"
  | "tether-high-contrast-light"
  | "mono-green"
  | "mono-amber";

export interface Theme {
  id: ThemeId;
  name: string;
  dark: boolean;
  terminal: TerminalColors;
  ui: UiColors & Partial<UiExtras>;
}
