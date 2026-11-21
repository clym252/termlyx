import { useCallback, useEffect, useRef, useState, type MouseEvent, type PointerEvent } from "react";
import { createPortal } from "react-dom";
import { CLICK_GRACE_MS, LONG_PRESS_MS } from "../app/useLongPressLabel";
import { RowMenu, inMenu, type MenuEntry } from "../ui/RowMenu";
import { usePaneDrag, type TouchDrag } from "./paneDrag";

/** How far a finger may drift during a hold before it is a drag, not a press. */
const HOLD_SLOP_PX = 8;

/**
 * A still 500 ms touch arms the pane's header; what it does is decided when
 * the finger moves or lifts, so a drag never also opens the menu:
 * - lifts without moving past the slop: `onHold` with the finger's position (the menu);
 * - moves past the slop: `drag` follows the finger and swaps on lift.
 * The click that ends such a press is swallowed.
 */
function useTouchHold(onHold: (at: { x: number; y: number }) => void, drag: TouchDrag) {
  const hold = useRef(onHold);
  hold.current = onHold;
  const dragRef = useRef(drag);
  dragRef.current = drag;
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const start = useRef({ x: 0, y: 0 });
  const fired = useRef(false);
  const armed = useRef(false);
  const dragging = useRef(false);
  const cancel = useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
  }, []);
  useEffect(() => cancel, [cancel]);
  // Like useLongPressLabel: the click arrives just after pointerup, so `fired` outlives the press briefly.
  const end = useCallback(() => {
    cancel();
    armed.current = false;
    dragging.current = false;
    setTimeout(() => {
      fired.current = false;
    }, CLICK_GRACE_MS);
  }, [cancel]);
  return {
    onPointerDown: (e: PointerEvent<HTMLElement>) => {
      fired.current = false;
      armed.current = false;
      dragging.current = false;
      cancel();
      if (e.pointerType !== "touch" || inMenu(e.target)) return;
      start.current = { x: e.clientX, y: e.clientY };
      timer.current = setTimeout(() => {
        timer.current = null;
        fired.current = true;
        armed.current = true;
      }, LONG_PRESS_MS);
    },
    onPointerMove: (e: PointerEvent<HTMLElement>) => {
      const far = Math.hypot(e.clientX - start.current.x, e.clientY - start.current.y) > HOLD_SLOP_PX;
      if (timer.current) {
        if (far) cancel(); // moved before the hold: a scroll or a swipe, not a press
        return;
      }
      if (!armed.current) return;
      if (!dragging.current) {
        if (!far) return;
        dragging.current = true;
        dragRef.current.begin(e.currentTarget);
      }
      dragRef.current.move(e.clientX, e.clientY);
    },
    onPointerUp: () => {
      if (dragging.current) dragRef.current.end();
      else if (armed.current) hold.current(start.current);
      end();
    },
    onPointerCancel: () => {
      if (dragging.current) dragRef.current.cancel();
      end();
    },
    onClickCapture: (e: MouseEvent<HTMLElement>) => {
      if (!fired.current || inMenu(e.target)) return;
      fired.current = false;
      e.preventDefault();
      e.stopPropagation();
    },
  };
}

/** What Workspace gives a pane's header besides its name. */
export interface PaneHeaderControls {
  /** The pane fills its tab: how many panes the tab has. */
  maximized: { count: number } | null;
  /** The pane's menu (see layoutMenu). Acts on the active pane, so opening it activates this one. */
  menu: MenuEntry[];
  onRestore: () => void;
  /** This pane trades places with `other` (a header dragged onto it, or this header dragged onto `other`). */
  onSwap: (other: string) => void;
}

interface PaneHeaderProps extends PaneHeaderControls {
  paneId: string;
  label: string;
  active: boolean;
  /** The tab broadcasts: how many panes typing reaches. */
  broadcast?: { count: number } | null;
  onActivate: () => void;
}

/**
 * A split pane's name bar, with its ⋯ menu. The menu also opens on a
 * right-click (at the pointer) and a 500 ms touch hold. Dragging the bar onto
 * another pane swaps the two (a mouse drag, or a touch hold, then drag).
 */
export function PaneHeader({ paneId, label, active, maximized, broadcast, menu, onActivate, onRestore, onSwap }: PaneHeaderProps) {
  const [at, setAt] = useState<{ x: number; y: number } | null>(null);
  const open = (point: { x: number; y: number }) => {
    onActivate();
    setAt(point);
  };
  const { props: dragProps, touch: touchDrag } = usePaneDrag(paneId, onSwap);
  const touch = useTouchHold(open, touchDrag);
  return (
    <div
      className="pane-header"
      data-active={active ? "true" : "false"}
      {...dragProps}
      {...touch}
      onContextMenu={(e) => {
        // A menu in a portal still bubbles through React to here.
        if (inMenu(e.target)) return;
        e.preventDefault();
        open({ x: e.clientX, y: e.clientY });
      }}
    >
      <span className="pane-title">
        {label}
        {maximized && ` · Maximized · ${maximized.count} ${maximized.count === 1 ? "pane" : "panes"}`}
      </span>
      {broadcast && (
        <span className="pane-broadcast">
          Typing goes to {broadcast.count} {broadcast.count === 1 ? "pane" : "panes"}
        </span>
      )}
      <span className="pane-header-actions">
        {maximized && (
          <button type="button" className="pane-restore" onClick={onRestore}>
            Restore
          </button>
        )}
        <button
          type="button"
          className="pane-menu"
          aria-label={`Pane menu for ${label}`}
          aria-haspopup="menu"
          aria-expanded={at !== null}
          data-menu-trigger={paneId}
          title="Pane menu"
          onClick={(e) => {
            if (at) return setAt(null);
            const box = e.currentTarget.getBoundingClientRect();
            open({ x: box.left, y: box.bottom + 2 });
          }}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" focusable="false">
            <circle cx="5.5" cy="12" r="1.75" />
            <circle cx="12" cy="12" r="1.75" />
            <circle cx="18.5" cy="12" r="1.75" />
          </svg>
        </button>
      </span>
      {/* In the body: the pane clips, and its stacking context sits under the panes after it. */}
      {at &&
        createPortal(
          <RowMenu owner={paneId} label="Pane menu" items={menu} anchor={at} onClose={() => setAt(null)} />,
          document.body,
        )}
    </div>
  );
}
