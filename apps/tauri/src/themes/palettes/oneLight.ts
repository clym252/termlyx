import type { Theme } from "../types";

/**
 * One Light, from navarasu/onedark.nvim (light style palette and terminal.lua mapping), MIT.
 * https://github.com/navarasu/onedark.nvim. The app chrome mapping is ours; see themes/README.md.
 */

export const oneLight: Theme = {
  id: "one-light",
  name: "One Light",
  dark: false,
  ui: {
    bg: "#fafafa",
    panel: "#f2f2f3",
    "panel-raised": "#e9e9e9",
    border: "#d3d4d5",
    text: "#383a42",
    muted: "#676870",
    accent: "#4078f2",
    "accent-text": "#000000",
    danger: "#c42b1d",
    "danger-bg": "#f7e1df",
    focus: "#4078f2",
  },
  terminal: {
    background: "#fafafa",
    foreground: "#383a42",
    cursor: "#383a42",
    cursorAccent: "#fafafa",
    selectionBackground: "#e6e6e6",
    black: "#101012",
    red: "#e45649",
    green: "#50a14f",
    yellow: "#986801",
    blue: "#4078f2",
    magenta: "#a626a4",
    cyan: "#0184bc",
    white: "#383a42",
    brightBlack: "#a0a1a7",
    brightRed: "#e45649",
    brightGreen: "#50a14f",
    brightYellow: "#986801",
    brightBlue: "#4078f2",
    brightMagenta: "#a626a4",
    brightCyan: "#0184bc",
    brightWhite: "#383a42",
  },
};
