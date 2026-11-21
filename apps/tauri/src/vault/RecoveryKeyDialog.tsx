import { useState } from "react";
import { Dialog } from "../ui/Dialog";

/**
 * Shows the recovery key. It is the only place the key is ever displayed:
 * the dialog cannot be dismissed except with "I saved it", and the caller
 * drops the key as soon as it closes.
 */
export function RecoveryKeyDialog({
  recoveryKey,
  onSaved,
}: {
  recoveryKey: string;
  onSaved: () => void;
}) {
  const [copy, setCopy] = useState<"idle" | "copied" | "failed">("idle");

  async function copyKey() {
    try {
      await navigator.clipboard.writeText(recoveryKey);
      setCopy("copied");
    } catch {
      setCopy("failed");
    }
  }

  return (
    <Dialog title="Your recovery key">
      <div className="dialog-content">
        <p>
          This key is the only way back into your vault if you forget the master password. Store it
          somewhere safe. <strong>It will not be shown again.</strong>
        </p>
        <label className="field">
          <span>Recovery key</span>
          <input
            className="mono"
            readOnly
            value={recoveryKey}
            spellCheck={false}
            onFocus={(e) => e.currentTarget.select()}
          />
        </label>
        <div className="row">
          <button type="button" onClick={() => void copyKey()}>
            Copy recovery key
          </button>
          <span role="status" className="muted">
            {copy === "copied" && "Copied"}
            {copy === "failed" && "Could not copy. Select the key and copy it by hand."}
          </span>
        </div>
      </div>
      <div className="dialog-actions">
        <button type="button" className="primary" onClick={onSaved}>
          I saved it
        </button>
      </div>
    </Dialog>
  );
}
