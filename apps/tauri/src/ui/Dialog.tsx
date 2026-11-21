import { useEffect, useId, useRef, type ReactNode, type RefObject } from "react";

interface DialogProps {
  title: string;
  /**
   * Called for Escape and a click on the backdrop. Omit for a dialog that
   * only its own buttons may close.
   */
  onClose?: () => void;
  children: ReactNode;
  wide?: boolean;
  /** Focused when the dialog opens (default: the first focusable element). */
  initialFocus?: RefObject<HTMLElement | null>;
}

/** A modal on the native `<dialog>`: focus is trapped and returned by the browser. */
export function Dialog({ title, onClose, children, wide, initialFocus }: DialogProps) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();

  useEffect(() => {
    const el = ref.current;
    if (el && !el.open) el.showModal();
    initialFocus?.current?.focus();
    // Only on open.
  }, []);

  // The webview scrolls a focused field into view before the software keyboard
  // shrinks the sheet, and never accounts for the pinned title and buttons: keep
  // the focused field between them on focus and on every viewport resize.
  useEffect(() => {
    const el = ref.current;
    const vv = window.visualViewport;
    if (!el) return;
    const reveal = () => {
      const body = el.querySelector<HTMLElement>(".dialog-body");
      const field = document.activeElement;
      if (!body || !(field instanceof HTMLElement) || !body.contains(field)) return;
      if (!field.matches("input, textarea, select")) return;
      const top = body.querySelector(".dialog-title")?.getBoundingClientRect().bottom ?? body.getBoundingClientRect().top;
      const actions = [...body.querySelectorAll(".dialog-actions")].pop();
      const bottom = actions?.getBoundingClientRect().top ?? body.getBoundingClientRect().bottom;
      const r = field.getBoundingClientRect();
      const gap = 8;
      if (r.bottom + gap > bottom) body.scrollTop += r.bottom + gap - bottom;
      else if (r.top - gap < top) body.scrollTop -= top - (r.top - gap);
    };
    el.addEventListener("focusin", reveal);
    vv?.addEventListener("resize", reveal);
    return () => {
      el.removeEventListener("focusin", reveal);
      vv?.removeEventListener("resize", reveal);
    };
  }, []);

  return (
    <dialog
      ref={ref}
      className={wide ? "dialog dialog-wide" : "dialog"}
      aria-labelledby={titleId}
      onCancel={(e) => {
        // React also delivers a nested dialog's cancel here (it follows the component
        // tree, not the DOM): Escape on that one must not close this one.
        if (e.target !== e.currentTarget) return;
        e.preventDefault();
        onClose?.();
      }}
      onClick={(e) => {
        // A click on the backdrop lands on the <dialog> element itself.
        if (e.target === e.currentTarget) onClose?.();
      }}
    >
      <div className="dialog-body">
        <h2 id={titleId} className="dialog-title">
          {title}
        </h2>
        {children}
      </div>
    </dialog>
  );
}

interface ConfirmDialogProps {
  title: string;
  confirmLabel: string;
  danger?: boolean;
  busy?: boolean;
  error?: string | null;
  onConfirm: () => void;
  onCancel: () => void;
  children: ReactNode;
}

export function ConfirmDialog(p: ConfirmDialogProps) {
  return (
    <Dialog title={p.title} onClose={p.busy ? undefined : p.onCancel}>
      <div className="dialog-content">{p.children}</div>
      {p.error && (
        <p role="alert" className="form-error">
          {p.error}
        </p>
      )}
      <div className="dialog-actions">
        <button type="button" onClick={p.onCancel} disabled={p.busy}>
          Cancel
        </button>
        <button
          type="button"
          className={p.danger ? "danger" : "primary"}
          onClick={p.onConfirm}
          disabled={p.busy}
        >
          {p.confirmLabel}
        </button>
      </div>
    </Dialog>
  );
}
