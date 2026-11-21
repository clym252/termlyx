import type { Theme } from "../types";

/**
 * Gruvbox, from morhetz/gruvbox colors/gruvbox.vim (its palette and terminal_color_* mapping), MIT/X11.
 * https://github.com/morhetz/gruvbox. The app chrome mapping is ours; see themes/README.md.
 */

/** Adjusted for contrast: red (lightness only; see themes/README.md). */
export const gruvboxDark: Theme = {
  id: "gruvbox-dark",
  name: "Gruvbox Dark",
  dark: true,
  ui: {
    bg: "#282828",
    panel: "#302f2e",
    "panel-raised": "#3a3834",
    border: "#4f4c44",
    text: "#ebdbb2",
    muted: "#ab9f93",
    accent: "#458588",
    "accent-text": "#000000",
    danger: "#ed8581",
    "danger-bg": "#432827",
    focus: "#83a598",
  },
  terminal: {
    background: "#282828",
    foreground: "#ebdbb2",
    cursor: "#ebdbb2",
    cursorAccent: "#282828",
    selectionBackground: "#504945",
    black: "#282828",
    red: "#de2720",
    green: "#98971a",
    yellow: "#d79921",
    blue: "#458588",
    magenta: "#b16286",
    cyan: "#689d6a",
    white: "#a89984",
    brightBlack: "#928374",
    brightRed: "#fb4934",
    brightGreen: "#b8bb26",
    brightYellow: "#fabd2f",
    brightBlue: "#83a598",
    brightMagenta: "#d3869b",
    brightCyan: "#8ec07c",
    brightWhite: "#ebdbb2",
  },
};

/** Adjusted for contrast: green, yellow, cyan (lightness only; see themes/README.md). */
export const gruvboxLight: Theme = {
  id: "gruvbox-light",
  name: "Gruvbox Light",
  dark: false,
  ui: {
    bg: "#fbf1c7",
    panel: "#f3eac1",
    "panel-raised": "#eae0ba",
    border: "#d5ccaa",
    text: "#3c3836",
    muted: "#6b5f54",
    accent: "#458588",
    "accent-text": "#000000",
    danger: "#b6201a",
    "danger-bg": "#f4d2ae",
    focus: "#076678",
  },
  terminal: {
    background: "#fbf1c7",
    foreground: "#3c3836",
    cursor: "#3c3836",
    cursorAccent: "#fbf1c7",
    selectionBackground: "#d5c4a1",
    black: "#fbf1c7",
    red: "#cc241d",
    green: "#8f8e19",
    yellow: "#b4801c",
    blue: "#458588",
    magenta: "#b16286",
    cyan: "#619563",
    white: "#7c6f64",
    brightBlack: "#928374",
    brightRed: "#9d0006",
    brightGreen: "#79740e",
    brightYellow: "#b57614",
    brightBlue: "#076678",
    brightMagenta: "#8f3f71",
    brightCyan: "#427b58",
    brightWhite: "#3c3836",
    ipHighlight: "#a35177",
  },
};
