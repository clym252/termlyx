import type { Theme } from "../types";

/**
 * Nord, from its public colour palette (https://www.nordtheme.com/docs/colors-and-palettes,
 * MIT). See themes/README.md.
 */
// Polar Night
const nord0 = "#2e3440";
const nord1 = "#3b4252";
const nord2 = "#434c5e";
const nord3 = "#4c566a";
// Snow Storm
const nord4 = "#d8dee9";
const nord5 = "#e5e9f0";
const nord6 = "#eceff4";
// Frost
const nord7 = "#8fbcbb";
const nord8 = "#88c0d0";
const nord9 = "#81a1c1";
// Aurora
const nord11 = "#bf616a";
const nord13 = "#ebcb8b";
const nord14 = "#a3be8c";
const nord15 = "#b48ead";

export const nord: Theme = {
  id: "nord",
  name: "Nord",
  dark: true,
  ui: {
    bg: nord0,
    panel: nord1,
    "panel-raised": nord2,
    border: nord3,
    // nord6: nord4 is too faint on the raised panel (nord2).
    text: nord6,
    // A solid mid grey between nord3 and nord4 (ours): the palette has none that reads on nord2.
    muted: "#b5bdcb",
    accent: nord8,
    "accent-text": nord0,
    // nord11 lightened to read on the panels, on a dark red ground (ours).
    danger: "#d89fa4",
    "danger-bg": "#3b2a30",
    focus: nord8,
  },
  terminal: {
    background: nord0,
    foreground: nord4,
    cursor: nord4,
    cursorAccent: nord0,
    selectionBackground: nord2,
    // The ANSI mapping Nord's terminal ports use.
    black: nord1,
    red: nord11,
    green: nord14,
    yellow: nord13,
    blue: nord9,
    magenta: nord15,
    cyan: nord8,
    white: nord5,
    brightBlack: nord3,
    brightRed: nord11,
    brightGreen: nord14,
    brightYellow: nord13,
    brightBlue: nord9,
    brightMagenta: nord15,
    brightCyan: nord7,
    brightWhite: nord6,
    // nord15 lightened slightly to 4.5:1 on nord0.
    ipHighlight: "#b691af",
  },
};
