/**
 * WCAG 2.x contrast, and the readability floors every theme must meet.
 */
import { ipHighlightOf, uiColorsOf } from ".";
import type { Theme } from "./types";

function rgb(hex: string): [number, number, number] {
  const n = parseInt(hex.slice(1), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

function luminance(hex: string): number {
  const [r, g, b] = rgb(hex).map((c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** The contrast ratio of two `#rrggbb` colours, 1 to 21. */
export function contrastRatio(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

function toHsl([r, g, b]: [number, number, number]): [number, number, number] {
  const [rr, gg, bb] = [r / 255, g / 255, b / 255];
  const max = Math.max(rr, gg, bb);
  const min = Math.min(rr, gg, bb);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  const h = max === rr ? (gg - bb) / d + (gg < bb ? 6 : 0) : max === gg ? (bb - rr) / d + 2 : (rr - gg) / d + 4;
  return [h / 6, s, l];
}

function fromHsl(h: number, s: number, l: number): string {
  const f = (n: number) => {
    const k = (n + h * 12) % 12;
    const a = s * Math.min(l, 1 - l);
    return Math.round(255 * (l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1))));
  };
  return `#${[f(0), f(8), f(4)].map((c) => c.toString(16).padStart(2, "0")).join("")}`;
}

/**
 * `color` unchanged if it already has `min` contrast against `against`; otherwise
 * its lightness moved in 1% steps (down if `darken`, else up), hue and saturation kept,
 * until it does.
 */
export function nudgeToContrast(color: string, against: string, min: number, darken: boolean): string {
  if (contrastRatio(color, against) >= min) return color;
  const [h, s, l0] = toHsl(rgb(color));
  for (let l = l0; l >= 0 && l <= 1; l += darken ? -0.01 : 0.01) {
    const c = fromHsl(h, s, l);
    if (contrastRatio(c, against) >= min) return c;
  }
  return darken ? "#000000" : "#ffffff";
}

const CHROMATIC = ["red", "green", "yellow", "blue", "magenta", "cyan"] as const;

/** Every pair of `t` below its floor, as `"<id>: <a> vs <b> <ratio> < <floor>"`; empty when it passes. */
export function themeContrastFailures(t: Theme): string[] {
  const term = t.terminal;
  const ui = uiColorsOf(t);
  const pairs: [string, string, string, string, number][] = [
    ["foreground", term.foreground, "background", term.background, 7],
    ["selection", term.selectionBackground, "foreground", term.foreground, 3],
    ["ipHighlight", ipHighlightOf(t), "background", term.background, 4.5],
    ...CHROMATIC.flatMap((c) => {
      const bright = `bright${c[0].toUpperCase()}${c.slice(1)}` as keyof typeof term;
      return [
        [c, term[c], "background", term.background, 3] as [string, string, string, string, number],
        [bright, term[bright], "background", term.background, 3] as [string, string, string, string, number],
      ];
    }),
    ...(["bg", "panel", "panel-raised"] as const).flatMap((s) => [
      ["text", ui.text, s, ui[s], 7] as [string, string, string, string, number],
      ["muted", ui.muted, s, ui[s], 4.5] as [string, string, string, string, number],
    ]),
    ["accent-text", ui["accent-text"], "accent", ui.accent, 4.5],
    ["accent", ui.accent, "panel", ui.panel, 3],
    ["danger", ui.danger, "panel", ui.panel, 4.5],
    ["danger", ui.danger, "danger-bg", ui["danger-bg"], 4.5],
    ["on-danger", ui["on-danger"], "danger", ui.danger, 4.5],
    ["broadcast", ui.broadcast, "panel", ui.panel, 3],
    ["warning", ui.warning, "panel", ui.panel, 4.5],
    ["warning", ui.warning, "bg", ui.bg, 4.5],
    ["on-warning", ui["on-warning"], "warning", ui.warning, 4.5],
    ["success", ui.success, "panel", ui.panel, 3],
    ...[1, 2, 3, 4, 5, 6, 7, 8].map(
      (n) =>
        [`badge-${n}-fg`, ui[`badge-${n}-fg` as keyof typeof ui], `badge-${n}-bg`, ui[`badge-${n}-bg` as keyof typeof ui], 4.5] as [
          string,
          string,
          string,
          string,
          number,
        ],
    ),
  ];
  return pairs.flatMap(([an, a, bn, b, floor]) => {
    const r = contrastRatio(a, b);
    return r >= floor ? [] : [`${t.id}: ${an} vs ${bn} ${r.toFixed(2)} < ${floor}`];
  });
}
