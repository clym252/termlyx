import { useEffect, useState } from "react";
import { SOFT_KEYBOARD_MIN_PX, keyboardHeight } from "../viewport";

export interface SoftKeyboard {
  /** A software keyboard is up (it covers more than `SOFT_KEYBOARD_MIN_PX`). */
  open: boolean;
  /** Its height in CSS pixels (0 when none is up). */
  height: number;
}

/** The software keyboard, from native code (Android) or `visualViewport`; never open without either. */
export function useSoftKeyboard(): SoftKeyboard {
  const [height, setHeight] = useState(keyboardHeight);
  useEffect(() => {
    const vv = window.visualViewport;
    const update = () => setHeight(keyboardHeight());
    update();
    vv?.addEventListener("resize", update);
    window.addEventListener("tether-ime", update);
    return () => {
      vv?.removeEventListener("resize", update);
      window.removeEventListener("tether-ime", update);
    };
  }, []);
  return { open: height > SOFT_KEYBOARD_MIN_PX, height };
}
