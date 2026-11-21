import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";

/**
 * Shown while the camera is open (iOS). The scanner draws the camera behind
 * the web view, so for the duration the page turns see-through (`data-scanning`
 * on <html>, see ui.css) and only this prompt and its Cancel button remain.
 *
 * It is a modal `<dialog>` of its own: in the top layer it sits above any
 * dialog that hosts the sync form (Settings → Connect to server), and everything
 * under it is inert, so Cancel is the one thing that can be tapped. Escape
 * cancels the scan too.
 */
export function QrScanOverlay({ onCancel }: { onCancel: () => void }) {
  const ref = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (el && !el.open) el.showModal();
    document.documentElement.dataset.scanning = "true";
    return () => {
      delete document.documentElement.dataset.scanning;
    };
  }, []);

  return createPortal(
    <dialog
      ref={ref}
      className="qr-scan"
      aria-label="Scan QR code"
      onCancel={(e) => {
        e.preventDefault();
        onCancel();
      }}
    >
      <p className="qr-scan-hint">Point the camera at the QR code on the other device.</p>
      <div className="qr-scan-frame" aria-hidden="true" />
      <button type="button" className="primary" onClick={onCancel}>
        Cancel scan
      </button>
    </dialog>,
    document.body,
  );
}
