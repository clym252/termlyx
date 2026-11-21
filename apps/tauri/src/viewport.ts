/**
 * Shrinks of the visual viewport by more than this are a software keyboard;
 * smaller ones are a hardware keyboard's shortcut strip or browser chrome.
 */
export const SOFT_KEYBOARD_MIN_PX = 120;

declare global {
  interface Window {
    /**
     * Android: CSS px the IME covers from the window's bottom, set by `MainActivity` (which then fires
     * `"tether-ime"` on `window`). The WebView never resizes there, so `visualViewport` cannot tell.
     */
    __TETHER_IME_PX__?: number;
  }
}

/**
 * How much of the window the software keyboard covers: what native code
 * reports (Android), else the layout viewport's height (which the keyboard
 * does not change on iOS) minus the visual viewport's. 0 without either.
 */
export function keyboardHeight(): number {
  const ime = window.__TETHER_IME_PX__;
  if (ime !== undefined) return Math.max(0, Math.round(ime));
  const vv = window.visualViewport;
  if (!vv) return 0;
  const layout = Math.max(window.innerHeight, document.documentElement.clientHeight);
  return Math.max(0, Math.round(layout - vv.height));
}

/**
 * iOS WKWebView does not shrink the layout viewport when the software
 * keyboard opens (`interactive-widget=resizes-content` is ignored), so the
 * bottom of the terminal ends up under the keyboard. Track the *visual*
 * viewport height in `--app-height` instead; `.app` sizes itself from it.
 * On Android the window's height less the IME native code reports.
 * The keyboard's height goes in `--keyboard-height`, and the root carries
 * `data-soft-keyboard` while one is up.
 * No-op where neither `visualViewport` (jsdom) nor native code says anything.
 */
export function trackVisualViewport(): () => void {
  const vv = window.visualViewport;
  const root = document.documentElement;
  const apply = () => {
    const kb = keyboardHeight();
    const height = window.__TETHER_IME_PX__ !== undefined ? window.innerHeight - kb : vv?.height;
    if (height === undefined) return;
    root.style.setProperty("--app-height", `${height}px`);
    root.style.setProperty("--keyboard-height", `${kb}px`);
    root.toggleAttribute("data-soft-keyboard", kb > SOFT_KEYBOARD_MIN_PX);
    // The webview may have scrolled the page to reveal the focused input.
    if (vv && vv.offsetTop !== 0) window.scrollTo(0, 0);
  };
  apply();
  vv?.addEventListener("resize", apply);
  vv?.addEventListener("scroll", apply);
  window.addEventListener("tether-ime", apply);
  return () => {
    vv?.removeEventListener("resize", apply);
    vv?.removeEventListener("scroll", apply);
    window.removeEventListener("tether-ime", apply);
    root.style.removeProperty("--app-height");
    root.style.removeProperty("--keyboard-height");
    root.removeAttribute("data-soft-keyboard");
  };
}
