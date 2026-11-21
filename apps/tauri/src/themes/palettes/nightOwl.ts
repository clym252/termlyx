import type { Theme } from "../types";

/**
 * Night Owl, from sdras/night-owl-vscode-theme (terminal.ansi* colours), MIT.
 * https://github.com/sdras/night-owl-vscode-theme. The app chrome mapping is ours; see themes/README.md.
 */

export const nightOwl: Theme = {
  id: "night-owl",
  name: "Night Owl",
  dark: true,
  ui: {
    bg: "#011627",
    panel: "#0a1e2f",
    "panel-raised": "#142839",
    border: "#2c3e4e",
    text: "#d6deeb",
    muted: "#8f8e8e",
    accent: "#82aaff",
    "accent-text": "#011627",
    danger: "#f05c59",
    "danger-bg": "#251f2d",
    focus: "#82aaff",
  },
  terminal: {
    background: "#011627",
    foreground: "#d6deeb",
    cursor: "#d6deeb",
    cursorAccent: "#011627",
    selectionBackground: "#1d3b53",
    black: "#011627",
    red: "#ef5350",
    green: "#22da6e",
    yellow: "#c5e478",
    blue: "#82aaff",
    magenta: "#c792ea",
    cyan: "#21c7a8",
    white: "#ffffff",
    brightBlack: "#575656",
    brightRed: "#ef5350",
    brightGreen: "#22da6e",
    brightYellow: "#ffeb95",
    brightBlue: "#82aaff",
    brightMagenta: "#c792ea",
    brightCyan: "#7fdbca",
    brightWhite: "#ffffff",
  },
};
