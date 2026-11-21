/**
 * Display-only highlighting of terminal output (IP addresses): xterm
 * decorations over the matched cells. The buffer, selection, copy and paste
 * are untouched.
 */
import type { IDecoration, IDisposable, IMarker, Terminal } from "@xterm/xterm";
import { ipHighlightOf, type Theme } from "../themes";

export interface Match {
  start: number;
  end: number;
}

const OCTET = "(?:25[0-5]|2[0-4]\\d|1\\d\\d|[1-9]?\\d)";
const IPV4 = new RegExp(`(?<![\\w.:])${OCTET}(?:\\.${OCTET}){3}(?:\\/(?:3[0-2]|[12]?\\d)|:\\d{1,5})?(?![\\w.]|:\\d)`, "g");
const IPV6_CANDIDATE = /(?<![\w:.%])[0-9A-Fa-f:]{3,}(?:%[\w.]+)?(?:\/\d{1,3})?(?![\w:])/g;

function validIpv6(text: string): boolean {
  const core = text.split(/[%/]/)[0];
  const halves = core.split("::");
  if (halves.length > 2 || !/\d/.test(core)) return false;
  const groups = halves.flatMap((h) => (h === "" ? [] : h.split(":")));
  if (!groups.every((g) => /^[0-9A-Fa-f]{1,4}$/.test(g))) return false;
  // Without "::" all eight groups are written (MACs and times have fewer).
  return halves.length === 2 ? groups.length <= 7 : groups.length === 8;
}

/** IPv4 (with an optional /prefix or :port) and IPv6 (optional %zone, /prefix) addresses in `line`. */
export function findIps(line: string): Match[] {
  const v6 = [...line.matchAll(IPV6_CANDIDATE)]
    .filter((m) => validIpv6(m[0]))
    .map((m) => ({ start: m.index, end: m.index + m[0].length }));
  const v4 = [...line.matchAll(IPV4)]
    .map((m) => ({ start: m.index, end: m.index + m[0].length }))
    .filter((a) => !v6.some((b) => a.start < b.end && b.start < a.end));
  return [...v4, ...v6].sort((a, b) => a.start - b.start);
}

/** What gets highlighted, and in which colour. Later rules (keywords, URLs) are added here. */
export const HIGHLIGHT_RULES: { name: string; find(line: string): Match[]; color(t: Theme): string }[] = [
  { name: "ip", find: findIps, color: ipHighlightOf },
];

/** Decorations on one buffer row, and the text they were made for. */
interface Row {
  marker: IMarker;
  text: string;
  decorations: IDecoration[];
}

/**
 * Highlights matches in `term`'s output, in `theme`'s colours (re-attach on
 * a theme change). Display only: decorations over the cells.
 *
 * After each write (at most once a frame) it checks the rows that may have
 * changed: those written since the last check and the bottom screenful,
 * where programs and `clear` redraw. A row whose text changed is redone.
 * Only the normal screen is highlighted: decorations are found by line
 * number in whichever buffer shows, so they are all dropped while a
 * full-screen program (the alternate screen) runs and made again after.
 * A resize reflows the text: everything is redone.
 */
export function attachHighlights(term: Terminal, theme: Theme): IDisposable {
  const rows = new Set<Row>();
  let mark: IMarker | undefined; // the cursor's row at the last check
  let timer: ReturnType<typeof setTimeout> | undefined;

  const drop = (row: Row) => {
    rows.delete(row);
    row.decorations.forEach((d) => d.dispose());
    row.marker.dispose();
  };
  const dropAll = () => {
    [...rows].forEach(drop);
    mark?.dispose();
    mark = undefined;
  };

  /** The row's text and the cell each character sits in (wide characters take two cells). */
  const read = (y: number): { text: string; cells: number[] } | null => {
    const line = term.buffer.active.getLine(y);
    if (!line) return null;
    let text = "";
    const cells: number[] = [];
    for (let x = 0; x < line.length; x++) {
      const cell = line.getCell(x);
      if (!cell || cell.getWidth() === 0) continue;
      const chars = cell.getChars() || " ";
      text += chars;
      for (let i = 0; i < chars.length; i++) cells.push(x);
    }
    return { text, cells };
  };

  // shortcut: one buffer row at a time, so an address wrapped onto the next row is missed; join wrapped rows if that bites.
  const redo = (y: number, byLine: Map<number, Row>) => {
    const got = read(y);
    const old = byLine.get(y);
    if (old && got && old.text === got.text) return;
    if (old) drop(old);
    if (!got) return;
    const buf = term.buffer.active;
    let row: Row | undefined;
    for (const rule of HIGHLIGHT_RULES) {
      for (const m of rule.find(got.text)) {
        row ??= { marker: term.registerMarker(y - (buf.baseY + buf.cursorY)), text: got.text, decorations: [] };
        const x = got.cells[m.start];
        const color = rule.color(theme);
        const d = term.registerDecoration({
          marker: row.marker,
          x,
          width: got.cells[m.end - 1] - x + 1,
          foregroundColor: color,
          layer: "top",
        });
        if (!d) continue;
        d.onRender((el) => {
          el.classList.add("term-highlight");
          el.style.color = color;
        });
        row.decorations.push(d);
      }
    }
    if (!row) return;
    const kept = row;
    rows.add(kept);
    // Trimmed off the top of the scrollback: gone with its line.
    kept.marker.onDispose(() => rows.delete(kept));
  };

  const check = () => {
    timer = undefined;
    const buf = term.buffer.active;
    if (buf.type !== "normal") {
      dropAll();
      return;
    }
    const byLine = new Map<number, Row>();
    for (const r of rows) if (!r.marker.isDisposed) byLine.set(r.marker.line, r);
    const from = Math.min(mark && !mark.isDisposed ? mark.line : 0, buf.baseY);
    const to = Math.min(buf.baseY + term.rows, buf.length) - 1;
    for (let y = from; y <= to; y++) redo(y, byLine);
    mark?.dispose();
    mark = term.registerMarker(0);
  };
  const schedule = () => {
    timer ??= setTimeout(check, 16);
  };

  check();
  const subs = [
    term.onWriteParsed(schedule),
    term.buffer.onBufferChange(schedule),
    term.onResize(() => {
      dropAll();
      schedule();
    }),
  ];
  return {
    dispose: () => {
      clearTimeout(timer);
      subs.forEach((s) => s.dispose());
      dropAll();
    },
  };
}
