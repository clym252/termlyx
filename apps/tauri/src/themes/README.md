# Themes

Thirty palettes, each used for the terminal (xterm's 16 ANSI colours,
foreground, background, cursor, selection) and for the app's colour
variables (`ui.css`). Nothing here comes from any other SSH client's theme
bundles; the third-party palettes are transcribed from their authors'
public specifications only.

| Theme | File | Source | Licence |
|---|---|---|---|
| Termlyx Dark | `palettes/tetherDark.ts` | Our own (the app's original colours). | Termlyx's |
| Termlyx Light | `palettes/tetherLight.ts` | Our own. | Termlyx's |
| Dracula | `palettes/dracula.ts` | The Dracula specification: https://draculatheme.com/spec (palette, "ANSI Terminal Colours: Dracula Classic", UI colours "Background Dark"/"Background Light") (repository: https://github.com/dracula/dracula-theme) | MIT (LICENSE in the repository) |
| Nord | `palettes/nord.ts` | The Nord palette nord0–nord15: https://www.nordtheme.com/docs/colors-and-palettes; ANSI mapping as in Nord's own terminal ports (repository: https://github.com/nordtheme/nord) | MIT (LICENSE in the repository) |
| Tokyo Night, Tokyo Night Storm, Tokyo Night Day | `palettes/tokyoNight.ts` | folke/tokyonight.nvim `extras/alacritty/*.toml`: https://github.com/folke/tokyonight.nvim | Apache-2.0 |
| Catppuccin Mocha, Catppuccin Macchiato, Catppuccin Latte | `palettes/catppuccin.ts` | catppuccin/alacritty: https://github.com/catppuccin/alacritty | MIT |
| Rosé Pine, Rosé Pine Moon | `palettes/rosePine.ts` | rose-pine/alacritty `dist`: https://github.com/rose-pine/alacritty | MIT |
| Kanagawa Wave, Kanagawa Dragon | `palettes/kanagawa.ts` | rebelot/kanagawa.nvim `extras/alacritty`: https://github.com/rebelot/kanagawa.nvim | MIT |
| Flexoki Dark, Flexoki Light | `palettes/flexoki.ts` | kepano/flexoki `kitty/flexoki_*.conf`: https://github.com/kepano/flexoki | MIT |
| Gruvbox Dark, Gruvbox Light | `palettes/gruvbox.ts` | morhetz/gruvbox `colors/gruvbox.vim` (palette and `terminal_color_*`): https://github.com/morhetz/gruvbox | MIT/X11 (README) |
| Everforest Dark, Everforest Light | `palettes/everforest.ts` | sainnhe/everforest `autoload/everforest.vim` (medium) and `colors/everforest.vim` terminal map: https://github.com/sainnhe/everforest | MIT |
| GitHub Dark, GitHub Light | `palettes/github.ts` | Primer primitives 7.17.1 (`canvas.default`, `fg.default`, `ansi.*`) as primer/github-vscode-theme maps them: https://github.com/primer/github-vscode-theme | MIT |
| Ayu Dark, Ayu Light | `palettes/ayu.ts` | ayu npm 8.0.1 editor and syntax colours (ayu-theme/ayu-colors); ANSI mapping ours: https://github.com/ayu-theme/ayu-colors | MIT |
| One Light | `palettes/oneLight.ts` | navarasu/onedark.nvim light palette and `terminal.lua`: https://github.com/navarasu/onedark.nvim | MIT |
| Night Owl | `palettes/nightOwl.ts` | sdras/night-owl-vscode-theme `terminal.ansi*`: https://github.com/sdras/night-owl-vscode-theme | MIT |
| Termlyx High Contrast Dark, Termlyx High Contrast Light | `palettes/tetherHighContrast.ts` | Our own. | Termlyx's |
| Mono Green, Mono Amber | `palettes/mono.ts` | Our own. | Termlyx's |

Notes on the mappings:

- The spec pages list colour values; the app-chrome mapping (which spec
  colour is the panel, border, muted text, accent) is ours.
- Every theme must meet the contrast floors in `contrast.ts` (checked by
  `contrast.test.ts`). The app chrome mapping is ours, so a failing chrome
  colour is simply chosen differently. A failing published terminal colour
  is nudged in lightness only (`nudgeToContrast`, hue and saturation kept)
  and the theme is marked **adjusted** below.
- Badge, broadcast and on-danger colours come from a shared dark or light
  set in `index.ts` unless a theme sets its own.
- Dracula: chrome muted text is the comment colour lightened (`#96a0c2`);
  danger background is a dark red of ours.
- Nord: chrome text is nord6 and muted text a solid grey of ours
  (`#b5bdcb`), since nord4 and the palette's greys are too faint on the
  raised panel; chrome danger is nord11 lightened (`#d89fa4`). IP highlight
  is nord15 lightened to `#b691af`.
- Selecting a theme in Settings → Terminal applies it to the app and to
  every open terminal. A host (or its group) whose `colorScheme` setting
  names a theme (by id or name, e.g. `nord` or `Termlyx Light`) uses it
  for its terminals instead.

## Adjusted terminal colours

These published colours fell below the floors and were moved in lightness
only (darker on light themes, lighter on dark ones) by the smallest step
that passes. The original values are in each source above.

- Tokyo Night Day: foreground, brightRed, brightBlue, brightMagenta.
- Catppuccin Mocha: selection.
- Catppuccin Macchiato: selection.
- Catppuccin Latte: green, yellow, magenta, brightGreen, brightYellow, brightMagenta.
- Flexoki Dark: red, blue, magenta.
- Flexoki Light: yellow, cyan.
- Gruvbox Dark: red.
- Gruvbox Light: green, yellow, cyan.
- Everforest Light: foreground, green, yellow, magenta, cyan, brightGreen, brightYellow, brightMagenta, brightCyan.
- Ayu Light: foreground, red, green, yellow, blue, cyan, brightRed, brightGreen, brightYellow, brightBlue, brightCyan.

Tokyo Night's terminal files give no selection colour; ours is its blue at
30% over the background.
