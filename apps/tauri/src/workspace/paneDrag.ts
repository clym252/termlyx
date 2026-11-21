import { useRef, type DragEvent, type PointerEvent } from "react";
import { inMenu } from "../ui/RowMenu";

/** What a dragged pane header carries (a tab strip drag carries its own type, so the two never mix). */
export const PANE_DRAG_TYPE = "application/x-tether-pane";

const TAB_PANEL = ".tab-panel";

/** The drag in flight: its pane and tab panel. `dataTransfer` data is unreadable until the drop, so this tells dragover. */
let dragging: { paneId: string; panel: Element | null } | null = null;

/** `data-pane-id` of the pane under the point, if it is inside `tabPanel` and is not `exclude`. */
export function paneUnder(x: number, y: number, tabPanel: HTMLElement, exclude: string): string | null {
  const pane = tabPanel.ownerDocument.elementFromPoint(x, y)?.closest<HTMLElement>("[data-pane-id]");
  if (!pane || !tabPanel.contains(pane)) return null;
  const id = pane.dataset.paneId ?? null;
  return id && id !== exclude ? id : null;
}

/** The one pane marked as the drop target (or none). Set on the element, not in state: a drag moves too fast to render. */
function markTarget(pane: Element | null) {
  for (const el of document.querySelectorAll("[data-drop-target]")) if (el !== pane) el.removeAttribute("data-drop-target");
  pane?.setAttribute("data-drop-target", "true");
}

/** What the touch hold in PaneHeader drives once the finger has moved past the hold's slop. */
export interface TouchDrag {
  begin: (from: HTMLElement) => void;
  move: (x: number, y: number) => void;
  /** The finger lifted: swaps with the marked pane, if any. */
  end: () => void;
  cancel: () => void;
}

/**
 * Props for a pane header that drags its pane onto another pane of the same
 * tab to trade places with it (`onSwap(other)`), plus the touch half: iPadOS
 * has no mouse drag from a header, so PaneHeader's 500 ms touch hold arms a
 * drag that follows the finger (`touch`).
 */
export function usePaneDrag(paneId: string, onSwap: (other: string) => void) {
  const swap = useRef(onSwap);
  swap.current = onSwap;
  const pointerType = useRef("mouse");
  const panel = useRef<HTMLElement | null>(null);
  const target = useRef<string | null>(null);

  const props = {
    draggable: true,
    // Capture: PaneHeader's own onPointerDown stays a separate prop.
    onPointerDownCapture: (e: PointerEvent<HTMLElement>) => {
      pointerType.current = e.pointerType;
    },
    onDragStart: (e: DragEvent<HTMLElement>) => {
      // iPadOS would lift the header with its own long-press drag, racing the touch hold.
      if (pointerType.current === "touch" || inMenu(e.target)) return e.preventDefault();
      e.dataTransfer.setData(PANE_DRAG_TYPE, paneId);
      e.dataTransfer.effectAllowed = "move";
      dragging = { paneId, panel: e.currentTarget.closest(TAB_PANEL) };
    },
    // Also fires when the drag is cancelled (Escape, dropped outside).
    onDragEnd: () => {
      dragging = null;
      markTarget(null);
    },
  };

  const touch: TouchDrag = {
    begin: (from) => {
      panel.current = from.closest<HTMLElement>(TAB_PANEL);
      target.current = null;
    },
    move: (x, y) => {
      target.current = panel.current ? paneUnder(x, y, panel.current, paneId) : null;
      markTarget(
        target.current ? (panel.current?.querySelector(`[data-pane-id="${CSS.escape(target.current)}"]`) ?? null) : null,
      );
    },
    end: () => {
      const other = target.current;
      touch.cancel();
      if (other) swap.current(other);
    },
    cancel: () => {
      target.current = null;
      panel.current = null;
      markTarget(null);
    },
  };
  return { props, touch };
}

/**
 * Drop handlers for a pane's root: a header of another pane of the same tab
 * dropped here trades places with this one (`onSwap(dragged pane)`). The root
 * gets `data-drop-target` while one is over it.
 */
export function usePaneDrop(paneId: string, onSwap: (dragged: string) => void) {
  const accepts = (e: DragEvent<HTMLElement>) =>
    e.dataTransfer.types.includes(PANE_DRAG_TYPE) &&
    dragging !== null &&
    dragging.paneId !== paneId &&
    dragging.panel === e.currentTarget.closest(TAB_PANEL);
  const over = (e: DragEvent<HTMLElement>) => {
    if (!accepts(e)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    markTarget(e.currentTarget);
  };
  return {
    onDragEnter: over,
    onDragOver: over,
    onDragLeave: (e: DragEvent<HTMLElement>) => {
      // Moving onto a child of the pane fires a leave on the pane too. Only this pane's mark goes: the next one's enter came first.
      if (e.relatedTarget instanceof Node && e.currentTarget.contains(e.relatedTarget)) return;
      e.currentTarget.removeAttribute("data-drop-target");
    },
    onDrop: (e: DragEvent<HTMLElement>) => {
      if (!accepts(e)) return;
      e.preventDefault();
      const from = dragging!.paneId;
      dragging = null;
      markTarget(null);
      onSwap(from);
    },
  };
}
