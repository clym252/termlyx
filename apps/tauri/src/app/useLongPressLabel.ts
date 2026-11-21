import { useCallback, useEffect, useRef, useState, type MouseEvent, type PointerEvent } from "react";

/** How long a finger rests on a rail icon before its name shows. */
export const LONG_PRESS_MS = 500;

/** How long after a press ends its click may still arrive (WebKit sends it a tick after pointerup). */
export const CLICK_GRACE_MS = 100;

export interface PressLabel {
  name: string;
  /** Viewport CSS px: the bubble's left edge and vertical centre. */
  x: number;
  y: number;
}

/**
 * Touch has no hover, so an icon-only control shows its name while a finger
 * rests on it (about half a second), beside the icon. The click that ends
 * such a press is swallowed: peeking at a name must not open the page.
 * Mouse and pen leave it to the `title` tooltip; VoiceOver reads aria-label.
 *
 * `label` is the bubble to draw (null while none); `bind(name)` gives the
 * handlers for one control.
 */
export function useLongPressLabel() {
  const [label, setLabel] = useState<PressLabel | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const shown = useRef(false);

  const stop = useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    setLabel(null);
  }, []);
  useEffect(() => stop, [stop]);
  // The press is over: hide the name now. The click that ends a long press
  // comes right after pointerup, so `shown` is cleared only once it has had
  // its chance (`CLICK_GRACE_MS`); a press that ends with no click (the finger dragged off,
  // pointercancel) must not swallow the next one.
  const end = useCallback(() => {
    stop();
    setTimeout(() => {
      shown.current = false;
    }, CLICK_GRACE_MS);
  }, [stop]);

  const bind = useCallback(
    (name: string) => ({
      onPointerDown: (e: PointerEvent<HTMLElement>) => {
        // A new press of any kind starts clean.
        shown.current = false;
        if (timer.current) clearTimeout(timer.current);
        timer.current = null;
        if (e.pointerType !== "touch") return;
        const box = e.currentTarget.getBoundingClientRect();
        timer.current = setTimeout(() => {
          shown.current = true;
          setLabel({ name, x: box.right + 8, y: box.top + box.height / 2 });
        }, LONG_PRESS_MS);
      },
      onPointerUp: end,
      onPointerCancel: end,
      onPointerLeave: end,
      onClickCapture: (e: MouseEvent<HTMLElement>) => {
        if (!shown.current) return;
        shown.current = false;
        e.preventDefault();
        e.stopPropagation();
      },
    }),
    [end],
  );
  return { label, bind };
}
