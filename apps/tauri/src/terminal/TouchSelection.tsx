import type { Terminal } from "@xterm/xterm";
import { useEffect, useLayoutEffect, useReducer, useRef, useState, type TouchEvent as ReactTouchEvent } from "react";
import {
  LongPress,
  MENU_CLOSED,
  attachLongPress,
  boundaryAt,
  cellAt,
  dragHandle,
  edgeDirection,
  gridOf,
  handlePoints,
  menuItems,
  menuReducer,
  placeMenu,
  selectRange,
  selectWord,
  selectionOf,
  type Grid,
  type HandleKind,
  type MenuItem,
} from "./touchSelect";
import { IS_ANDROID } from "../platform";

/** While a dragged handle is past the screen's top or bottom edge, the view scrolls a line this often (ms). */
export const EDGE_SCROLL_MS = 70;
/** Half a long press's menu is kept clear of the finger (px): it sits above the fingertip. */
const FINGER_PX = 28;

interface TouchSelectionProps {
  term: Terminal;
  /** The element the terminal is drawn in (its touches). */
  host: HTMLElement;
  /** The positioned box the menu and handles are drawn in (the terminal's stage). */
  stage: HTMLElement;
  /** Paste: the clipboard into the session (as typed). */
  onPaste(): void;
  /** Copy: the selected text to the clipboard. Handed on at once, never kept. */
  onCopy(text: string): void;
  /** Save as snippet: the selected text into a new snippet's editor. */
  onSaveSnippet(text: string): void;
}

/** A handle being dragged: which one, and where the finger is from the point it moves. */
interface Drag {
  which: HandleKind;
  dx: number;
  dy: number;
  x: number;
  y: number;
}

/**
 * The iOS terminal's touch copy and paste (see touchSelect.ts): a long-press
 * menu and a selection with two drag handles, drawn over the terminal.
 */
export function TouchSelection({ term, host, stage, onPaste, onCopy, onSaveSnippet }: TouchSelectionProps) {
  const [menu, dispatch] = useReducer(menuReducer, MENU_CLOSED);
  const menuRef = useRef(menu);
  menuRef.current = menu;
  // Redraws the handles and the menu where the selection is now (scrolling, output, resize).
  const [, setTick] = useState(0);
  const redraw = () => setTick((n) => n + 1);

  useEffect(() => {
    const lp = new LongPress(
      (x, y) => {
        const g = gridOf(term);
        if (!g) return;
        term.clearSelection();
        dispatch({ type: "press", x, y, cell: cellAt(g, x, y) });
      },
      () => {
        if (menuRef.current.kind === "closed") return false;
        term.clearSelection();
        dispatch({ type: "tap" });
        return true;
      },
    );
    return attachLongPress(host, lp);
  }, [term, host]);

  // Android's WebView has its own long-press selection and menu: only this menu may show.
  useEffect(() => {
    if (!IS_ANDROID) return;
    const cancel = (e: Event) => e.preventDefault();
    host.addEventListener("contextmenu", cancel);
    host.addEventListener("selectstart", cancel);
    return () => {
      host.removeEventListener("contextmenu", cancel);
      host.removeEventListener("selectstart", cancel);
    };
  }, [host]);

  // The selection gone from under the menu (new output, the screen switched, a key typed): the menu goes too.
  useEffect(() => {
    const sub = term.onSelectionChange(() => {
      if (menuRef.current.kind === "selection" && !term.hasSelection()) dispatch({ type: "close" });
      else redraw();
    });
    return () => sub.dispose();
  }, [term]);

  const open = menu.kind !== "closed";
  useEffect(() => {
    if (!open) return;
    const subs = [term.onRender(redraw), term.onResize(redraw)];
    return () => subs.forEach((s) => s.dispose());
  }, [term, open]);

  // Escape (and Android's Back, which sends it) puts the menu and the selection away, like every popup menu.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      term.clearSelection();
      dispatch({ type: "close" });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [term, open]);

  // Dragging a handle.
  const drag = useRef<Drag | null>(null);
  const edgeTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const stopEdge = () => {
    if (edgeTimer.current !== null) clearInterval(edgeTimer.current);
    edgeTimer.current = null;
  };
  useEffect(() => stopEdge, []);
  /** Moves the dragged handle to the finger; returns where the finger is past an edge. */
  const follow = (): -1 | 0 | 1 => {
    const d = drag.current;
    const g = gridOf(term);
    const sel = selectionOf(term);
    if (!d || !g || !sel) return 0;
    const x = d.x - d.dx;
    const y = d.y - d.dy;
    selectRange(term, dragHandle(sel, d.which, boundaryAt(g, x, y), g.cols));
    return edgeDirection(g, y);
  };
  const track = () => {
    const dir = follow();
    if (dir === 0) {
      stopEdge();
      return;
    }
    if (edgeTimer.current !== null) return;
    edgeTimer.current = setInterval(() => {
      const g = gridOf(term);
      const d = drag.current;
      const now = g && d ? edgeDirection(g, d.y - d.dy) : 0;
      if (now === 0) {
        stopEdge();
        return;
      }
      term.scrollLines(now);
      follow();
    }, EDGE_SCROLL_MS);
  };
  const handleStart = (which: HandleKind) => (e: ReactTouchEvent) => {
    const g = gridOf(term);
    const sel = selectionOf(term);
    const t = e.touches[0];
    if (!g || !sel || !t || e.touches.length !== 1) return;
    const p = handlePoints(g, sel)[which];
    // The finger moves the point it grabbed: the middle of the row at the selection's edge.
    drag.current = { which, dx: t.clientX - p.x, dy: t.clientY - (p.top + p.bottom) / 2, x: t.clientX, y: t.clientY };
    dispatch({ type: "dragStart" });
  };
  const handleMove = (e: ReactTouchEvent) => {
    const d = drag.current;
    const t = e.touches[0];
    if (!d || !t) return;
    d.x = t.clientX;
    d.y = t.clientY;
    track();
  };
  const handleEnd = (e: ReactTouchEvent) => {
    if (!drag.current) return;
    if (e.cancelable) e.preventDefault();
    drag.current = null;
    stopEdge();
    dispatch({ type: "dragEnd" });
  };

  const choose = (item: MenuItem) => {
    const m = menuRef.current;
    switch (item) {
      case "Paste":
        dispatch({ type: "close" });
        onPaste();
        break;
      case "Select":
        if (m.kind !== "actions") return;
        selectWord(term, m.cell);
        dispatch({ type: "selected" });
        break;
      case "Select All":
        term.selectAll();
        dispatch({ type: "selected" });
        break;
      case "Copy": {
        const text = term.getSelection();
        term.clearSelection();
        dispatch({ type: "close" });
        if (text) onCopy(text);
        break;
      }
      case "Save as snippet": {
        const text = term.getSelection();
        term.clearSelection();
        dispatch({ type: "close" });
        if (text) onSaveSnippet(text);
        break;
      }
      case "Done":
        term.clearSelection();
        dispatch({ type: "close" });
        break;
    }
  };

  // Geometry, in the stage's frame.
  const g = open ? gridOf(term) : null;
  const sel = menu.kind === "selection" && g ? selectionOf(term) : null;
  const box = stage.getBoundingClientRect();
  const handles = sel && g ? handlePoints(g, sel) : null;
  const anchor = menuAnchor(menu, g, handles);

  const menuEl = useRef<HTMLDivElement>(null);
  const [menuAt, setMenuAt] = useState<{ x: number; y: number } | null>(null);
  const items = menuItems(menu);
  const ax = anchor?.x;
  const atop = anchor?.top;
  const abottom = anchor?.bottom;
  useLayoutEffect(() => {
    const el = menuEl.current;
    if (!el || ax === undefined || atop === undefined || abottom === undefined) {
      setMenuAt(null);
      return;
    }
    const b = stage.getBoundingClientRect();
    const at = placeMenu(
      { x: ax, top: atop, bottom: abottom },
      { w: el.offsetWidth, h: el.offsetHeight },
      { left: b.left, top: b.top, right: b.right, bottom: b.bottom },
    );
    setMenuAt({ x: at.x - b.left, y: at.y - b.top });
  }, [ax, atop, abottom, items.length, stage]);

  return (
    <>
      {handles &&
        g &&
        (["start", "end"] as const).map((which) => {
          const p = handles[which];
          if (!p.visible) return null;
          return (
            <div
              key={which}
              className="touch-handle"
              data-handle={which}
              aria-hidden="true"
              style={{
                left: p.x - box.left,
                top: p.top - box.top,
                height: p.bottom - p.top,
              }}
              onTouchStart={handleStart(which)}
              onTouchMove={handleMove}
              onTouchEnd={handleEnd}
              onTouchCancel={handleEnd}
              onMouseDown={(e) => e.preventDefault()}
            />
          );
        })}
      {items.length > 0 && (
        <div
          ref={menuEl}
          className="touch-menu"
          role="menu"
          aria-label="Terminal text"
          style={menuAt ? { left: menuAt.x, top: menuAt.y } : { visibility: "hidden", left: 0, top: 0 }}
        >
          {items.map((item) => (
            <button
              key={item}
              type="button"
              role="menuitem"
              // Keeps the focus (and the software keyboard) where it is.
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => choose(item)}
            >
              {item}
            </button>
          ))}
        </div>
      )}
    </>
  );
}

/** What the menu sits over (client px): the finger for the actions, the selection for its own menu. */
function menuAnchor(
  menu: ReturnType<typeof menuReducer>,
  g: Grid | null,
  handles: ReturnType<typeof handlePoints> | null,
): { x: number; top: number; bottom: number } | null {
  if (menu.kind === "actions") return { x: menu.x, top: menu.y - FINGER_PX, bottom: menu.y + FINGER_PX };
  if (menu.kind !== "selection" || !g || !handles) return null;
  const { start, end } = handles;
  const screenBottom = g.top + g.height;
  // Over the selection's first row; below its last when there is no room above (placeMenu); the screen's edge for an end off it.
  const top = start.visible ? start.top : g.top;
  const bottom = end.visible ? end.bottom : screenBottom;
  const x = start.visible && end.visible && start.top === end.top ? (start.x + end.x) / 2 : g.left + g.width / 2;
  // Room for the handles' knobs.
  return { x, top: top - HANDLE_KNOB_PX, bottom: bottom + HANDLE_KNOB_PX };
}

/** How far a handle's knob reaches past its row (px; matches .touch-handle in ui.css). */
const HANDLE_KNOB_PX = 14;
