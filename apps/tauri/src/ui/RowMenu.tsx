import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent } from "react";
import type { Id } from "../api/types";

/** Whether an event started inside an open menu (a card's menu sits within the card, so its events reach the card). */
export const inMenu = (target: EventTarget) => target instanceof Element && target.closest('[role="menu"]') !== null;

/**
 * One entry: a plain `[label, run]`, or an object for a disabled, checkable or
 * shortcut-hinted one; "separator" draws a rule.
 */
export type MenuEntry =
  | [string, () => void]
  | { label: string; run: () => void; disabled?: boolean; checked?: boolean; hint?: string }
  | "separator";

const ENABLED = 'button:not([aria-disabled="true"])';

/** The gap kept between an anchored menu and the window's edges. */
const EDGE = 4;

/**
 * A small menu under a row or card; Escape, a click elsewhere or a choice closes it.
 * Without `anchor` it hangs under its positioned parent; with one it sits at that
 * viewport point (a right-click, or the card's corner for the keyboard).
 */
export function RowMenu({
  owner,
  label,
  items,
  onClose,
  anchor,
}: {
  /** The row or card whose "⋯" opened it. */
  owner: Id;
  label: string;
  items: MenuEntry[];
  onClose: () => void;
  anchor?: { x: number; y: number };
}) {
  const ref = useRef<HTMLUListElement>(null);
  // Callers pass a fresh closure each render; the effect below must not rerun (and refocus) for that.
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    ref.current?.querySelector<HTMLButtonElement>(ENABLED)?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      close.current();
      // Keyboard users land back on the button that opened it.
      document.querySelector<HTMLElement>(`[data-menu-trigger="${CSS.escape(owner)}"]`)?.focus();
    };
    const onDown = (e: MouseEvent) => {
      const target = e.target as Element;
      // The row's own "⋯" toggles the menu itself (its click closes it).
      if (target.closest?.(`[data-menu-trigger="${CSS.escape(owner)}"]`)) return;
      if (ref.current && !ref.current.contains(target)) close.current();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("mousedown", onDown);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("mousedown", onDown);
    };
  }, [owner]);
  // An anchored menu is moved, once measured, to stay inside the window: shifted left, flipped above.
  const [placed, setPlaced] = useState(anchor);
  const ax = anchor?.x;
  const ay = anchor?.y;
  useLayoutEffect(() => {
    if (ax === undefined || ay === undefined || !ref.current) return;
    const { width, height } = ref.current.getBoundingClientRect();
    const x = Math.max(EDGE, Math.min(ax, window.innerWidth - width - EDGE));
    const y = ay + height > window.innerHeight - EDGE ? Math.max(EDGE, ay - height) : ay;
    setPlaced({ x, y });
  }, [ax, ay]);
  const at = anchor && placed;
  const style: CSSProperties | undefined = at
    ? { position: "fixed", top: at.y, left: at.x, right: "auto", margin: 0 }
    : undefined;
  // Arrow keys walk the entries that can be chosen; a disabled one is passed over.
  const onKeyDown = (e: ReactKeyboardEvent<HTMLUListElement>) => {
    const keys = ["ArrowDown", "ArrowUp", "Home", "End"];
    if (!keys.includes(e.key) || !ref.current) return;
    const buttons = [...ref.current.querySelectorAll<HTMLButtonElement>(ENABLED)];
    if (buttons.length === 0) return;
    e.preventDefault();
    const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const step = e.key === "ArrowDown" ? 1 : -1;
    // Off the entries (at -1): Down goes to the first, Up to the last.
    const next =
      e.key === "Home" || (at < 0 && step > 0)
        ? 0
        : e.key === "End" || at < 0
          ? buttons.length - 1
          : (at + step + buttons.length) % buttons.length;
    buttons[next].focus();
  };
  return (
    <ul className="row-menu" role="menu" aria-label={label} ref={ref} style={style} onKeyDown={onKeyDown}>
      {items.map((entry, i) => {
        if (entry === "separator") return <li key={`sep-${i}`} role="separator" className="row-menu-separator" />;
        const { label: text, run, disabled, checked, hint } = Array.isArray(entry)
          ? { label: entry[0], run: entry[1], disabled: false, checked: undefined, hint: undefined }
          : entry;
        return (
          <li key={text} role="none">
            <button
              type="button"
              role={checked === undefined ? "menuitem" : "menuitemcheckbox"}
              aria-checked={checked}
              aria-disabled={disabled ? "true" : undefined}
              className={text.startsWith("Delete") ? "danger-text" : undefined}
              onClick={() => {
                if (disabled) return;
                onClose();
                run();
              }}
            >
              {hint ? (
                <>
                  <span>{text}</span>
                  <kbd>{hint}</kbd>
                </>
              ) : (
                text
              )}
            </button>
          </li>
        );
      })}
    </ul>
  );
}
