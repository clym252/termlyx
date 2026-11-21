/** The platform the webview runs on; UI words, shortcut labels and the layout follow it. */
export type Platform = "mac" | "windows" | "ios" | "android" | "other";

/** What kind of device: every platform but iOS and Android is a desktop. */
export type FormFactor = "desktop" | "phone" | "tablet";

declare global {
  interface Window {
    /** `std::env::consts::OS`, set by the backend before any page script runs (`platform_plugin` in `lib.rs`). */
    __TETHER_OS__?: string;
  }
}

/**
 * The platform the backend's OS names, else what a `navigator.platform` value
 * names (the browser mock and tests). The OS wins: iPadOS reports `MacIntel`.
 */
export function detectPlatform(os: string | undefined, navPlatform: string): Platform {
  switch (os) {
    case "macos":
      return "mac";
    case "windows":
      return "windows";
    case "ios":
      return "ios";
    case "android":
      return "android";
    case undefined:
      break;
    default:
      return "other";
  }
  if (/iPhone|iPad/i.test(navPlatform)) return "ios";
  if (/Mac/i.test(navPlatform)) return "mac";
  if (/^Win/i.test(navPlatform)) return "windows";
  return "other";
}

/** Phone or tablet on iOS and Android by the screen's short side in points (an iPad mini is 744); a desktop elsewhere. */
export function detectFormFactor(platform: Platform, shortSide: number): FormFactor {
  if (platform !== "ios" && platform !== "android") return "desktop";
  return shortSide >= 600 ? "tablet" : "phone";
}

/**
 * The layout for a form factor at a viewport width: an iPad in Slide Over
 * or a narrow Split View gets the phone layout; a desktop window never does,
 * however narrow.
 */
export function layoutFor(ff: FormFactor, viewportWidth: number): FormFactor {
  if (ff === "tablet" && viewportWidth < 600) return "phone";
  return ff;
}

/** Whether shortcuts use ⌘ (macOS, and iOS with a hardware keyboard) rather than Ctrl. */
export function usesCmdKey(platform: Platform): boolean {
  return platform === "mac" || platform === "ios";
}

/**
 * Fixed at load. Tests set `window.__TETHER_OS__` (or `navigator.platform`)
 * and the screen size before the app's scripts run; there is no product
 * switch.
 */
export const PLATFORM: Platform =
  typeof window !== "undefined" ? detectPlatform(window.__TETHER_OS__, navigator.platform) : "other";
export const IS_WINDOWS = PLATFORM === "windows";
export const IS_IOS = PLATFORM === "ios";
export const IS_ANDROID = PLATFORM === "android";
/** A phone or tablet: touch first, no local shell. */
export const IS_MOBILE = IS_IOS || IS_ANDROID;
/** Signed self-update: releases carry updates for macOS and Windows only (not Linux, iOS or Android). */
export const UPDATES_SUPPORTED = PLATFORM === "mac" || IS_WINDOWS;
/** ⌘ shortcuts and labels on this platform. */
export const CMD_KEYS = usesCmdKey(PLATFORM);
export const FORM_FACTOR: FormFactor = detectFormFactor(
  PLATFORM,
  typeof screen !== "undefined" ? Math.min(screen.width, screen.height) : 0,
);
/** What the device is called in UI words: iPhone, iPad, phone, tablet, or computer. */
export const DEVICE_NOUN: string = IS_IOS
  ? FORM_FACTOR === "tablet"
    ? "iPad"
    : "iPhone"
  : IS_ANDROID
    ? FORM_FACTOR === "tablet"
      ? "tablet"
      : "phone"
    : "computer";
