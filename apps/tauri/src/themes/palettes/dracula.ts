import type { Theme } from "../types";

/**
 * Dracula, from its public specification (https://draculatheme.com/spec,
 * MIT). See themes/README.md.
 */
const background = "#282a36";
const selection = "#44475a";
const foreground = "#f8f8f2";
const comment = "#6272a4";
const cyan = "#8be9fd";
const green = "#50fa7b";
const pink = "#ff79c6";
const purple = "#bd93f9";
const red = "#ff5555";
const yellow = "#f1fa8c";
/** The spec's UI colours "Background Dark" (also its ANSI black) and "Background Light". */
const backgroundDark = "#21222c";
const backgroundLight = "#343746";

export const dracula: Theme = {
  id: "dracula",
  name: "Dracula",
  dark: true,
  ui: {
    bg: background,
    panel: backgroundDark,
    "panel-raised": backgroundLight,
    border: selection,
    text: foreground,
    // The spec's comment colour, lightened to read on every panel (ours).
    muted: "#96a0c2",
    accent: purple,
    "accent-text": background,
    danger: red,
    "danger-bg": "#3a2229",
    focus: cyan,
  },
  terminal: {
    background,
    foreground,
    cursor: foreground,
    cursorAccent: background,
    selectionBackground: selection,
    // The spec's ANSI terminal colours.
    black: backgroundDark,
    red,
    green,
    yellow,
    blue: purple,
    magenta: pink,
    cyan,
    white: foreground,
    brightBlack: comment,
    brightRed: "#ff6e6e",
    brightGreen: "#69ff94",
    brightYellow: "#ffffa5",
    brightBlue: "#d6acff",
    brightMagenta: "#ff92df",
    brightCyan: "#a4ffff",
    brightWhite: "#ffffff",
  },
};
