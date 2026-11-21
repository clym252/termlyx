import type { Theme } from "../types";

/**
 * Flexoki, from kepano/flexoki kitty/flexoki_{dark,light}.conf, MIT.
 * https://github.com/kepano/flexoki. The app chrome mapping is ours; see themes/README.md.
 */

/** Adjusted for contrast: red, blue, magenta (lightness only; see themes/README.md). */
export const flexokiDark: Theme = {
  id: "flexoki-dark",
  name: "Flexoki Dark",
  dark: true,
  ui: {
    bg: "#100f0f",
    panel: "#181716",
    "panel-raised": "#21201f",
    border: "#363533",
    text: "#cecdc3",
    muted: "#898882",
    accent: "#2365b3",
    "accent-text": "#ffffff",
    danger: "#d9625b",
    "danger-bg": "#281413",
    focus: "#4385be",
  },
  terminal: {
    background: "#100f0f",
    foreground: "#cecdc3",
    cursor: "#cecdc3",
    cursorAccent: "#100f0f",
    selectionBackground: "#403e3c",
    black: "#100f0f",
    red: "#b3312a",
    green: "#66800b",
    yellow: "#ad8301",
    blue: "#2160aa",
    magenta: "#a83174",
    cyan: "#24837b",
    white: "#878580",
    brightBlack: "#6f6e69",
    brightRed: "#d14d41",
    brightGreen: "#879a39",
    brightYellow: "#d0a215",
    brightBlue: "#4385be",
    brightMagenta: "#ce5d97",
    brightCyan: "#3aa99f",
    brightWhite: "#cecdc3",
  },
};

/** Adjusted for contrast: yellow, cyan (lightness only; see themes/README.md). */
export const flexokiLight: Theme = {
  id: "flexoki-light",
  name: "Flexoki Light",
  dark: false,
  ui: {
    bg: "#fffcf0",
    panel: "#f5f3e7",
    "panel-raised": "#e9e7dc",
    border: "#cfcdc3",
    text: "#100f0f",
    muted: "#676662",
    accent: "#4385be",
    "accent-text": "#100f0f",
    danger: "#b8382c",
    "danger-bg": "#f8e2d6",
    focus: "#205ea6",
  },
  terminal: {
    background: "#fffcf0",
    foreground: "#100f0f",
    cursor: "#100f0f",
    cursorAccent: "#fffcf0",
    selectionBackground: "#cecdc3",
    black: "#100f0f",
    red: "#d14d41",
    green: "#879a39",
    yellow: "#b48c12",
    blue: "#4385be",
    magenta: "#ce5d97",
    cyan: "#37a198",
    white: "#fffcf0",
    brightBlack: "#6f6e69",
    brightRed: "#af3029",
    brightGreen: "#66800b",
    brightYellow: "#ad8301",
    brightBlue: "#205ea6",
    brightMagenta: "#a02f6f",
    brightCyan: "#24837b",
    brightWhite: "#f2f0e5",
    ipHighlight: "#c64285",
  },
};
