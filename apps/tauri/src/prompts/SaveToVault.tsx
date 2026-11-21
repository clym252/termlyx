import { useId, type RefObject } from "react";
import type { SaveTarget } from "../api/types";

/**
 * "Save password to vault" for a login prompt that offers it: unchecked by
 * default, naming the identity the password goes to. Uncontrolled; the
 * dialog reads `checked` at submit.
 */
export function SaveToVault({ target, checkbox }: { target: SaveTarget; checkbox: RefObject<HTMLInputElement | null> }) {
  const hintId = useId();
  return (
    <div className="field">
      <label className="choice">
        <input type="checkbox" ref={checkbox} aria-describedby={hintId} />
        <span>Save password to vault</span>
      </label>
      <p id={hintId} className="muted small">
        {target.isNew
          ? `Saved as this host's own login, after the login succeeds.`
          : `Saved on identity “${target.identityLabel}”, after the login succeeds.`}
      </p>
    </div>
  );
}
