import { useEffect, useRef, type ReactNode } from "react";

/** A swipe that opens the drawer starts this close to the left edge (CSS px)... */
export const EDGE_PX = 20;
/** ...and travels at least this far to the right; a swipe back to close travels as far to the left. */
export const SWIPE_PX = 60;

interface DrawerProps {
  open: boolean;
  /** A swipe in from the left edge asks to open (☰ opens it through the caller). */
  onOpen(): void;
  /** A tap outside the panel, Escape, or a swipe back to the left. */
  onClose(): void;
  children: ReactNode;
}

/**
 * The phone layout's menu: the sidebar's items in a panel over the page,
 * from the left. Opens on ☰ (the caller) or a swipe in from the left edge;
 * closes on a tap outside, Escape, a swipe back, or (the caller) on
 * navigating.
 */
export function Drawer({ open, onOpen, onClose, children }: DrawerProps) {
  const panel = useRef<HTMLDivElement>(null);
  // The handlers below are set up once; the latest props are read through these.
  const latest = useRef({ open, onOpen, onClose });
  latest.current = { open, onOpen, onClose };

  useEffect(() => {
    // Where the current one-finger touch started, and whether it already acted.
    let start: { x: number; y: number; done: boolean } | null = null;
    const onStart = (e: TouchEvent) => {
      const t = e.touches[0];
      start = e.touches.length === 1 && t ? { x: t.clientX, y: t.clientY, done: false } : null;
    };
    const onMove = (e: TouchEvent) => {
      const t = e.touches[0];
      if (!start || start.done || !t) return;
      const dx = t.clientX - start.x;
      const dy = t.clientY - start.y;
      // A scroll is mostly vertical: not a swipe.
      if (Math.abs(dy) > Math.abs(dx)) return;
      const { open: isOpen, onOpen: openIt, onClose: closeIt } = latest.current;
      if (!isOpen && start.x < EDGE_PX && dx > SWIPE_PX) {
        start.done = true;
        openIt();
      } else if (isOpen && dx < -SWIPE_PX) {
        start.done = true;
        closeIt();
      }
    };
    const onEnd = () => {
      start = null;
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || !latest.current.open) return;
      // A dialog opened from the drawer (search) owns Escape.
      if (document.querySelector("dialog[open]")) return;
      e.preventDefault();
      latest.current.onClose();
    };
    window.addEventListener("touchstart", onStart, { passive: true });
    window.addEventListener("touchmove", onMove, { passive: true });
    window.addEventListener("touchend", onEnd);
    window.addEventListener("touchcancel", onEnd);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("touchstart", onStart);
      window.removeEventListener("touchmove", onMove);
      window.removeEventListener("touchend", onEnd);
      window.removeEventListener("touchcancel", onEnd);
      window.removeEventListener("keydown", onKey);
    };
  }, []);

  // The focus moves into the panel as it opens, and back to what had it (☰) as it closes.
  useEffect(() => {
    if (!open) return;
    const before = document.activeElement as HTMLElement | null;
    panel.current?.querySelector<HTMLElement>("button:not(:disabled)")?.focus();
    return () => {
      if (before?.isConnected) before.focus();
    };
  }, [open]);

  if (!open) return null;
  return (
    <div className="drawer">
      <div className="drawer-backdrop" data-testid="drawer-backdrop" aria-hidden="true" onClick={onClose} />
      <div className="drawer-panel" role="dialog" aria-modal="true" aria-label="Menu" ref={panel}>
        {children}
      </div>
    </div>
  );
}
