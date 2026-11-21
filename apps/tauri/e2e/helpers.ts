import { expect, type Locator, type Page } from "@playwright/test";

export const PASSWORD = "correct horse battery";

/** Workspace chords for a platform (ruling R30; see src/workspace/shortcuts.ts). */
export function workspaceKeys(mac: boolean) {
  return mac
    ? {
        newTab: "Meta+T",
        close: "Meta+W",
        splitRight: "Meta+D",
        splitDown: "Meta+Shift+D",
        focus: (arrow: string) => `Meta+Alt+${arrow}`,
        localTerminal: "Meta+Shift+L",
        zoom: "Meta+Shift+Enter",
        broadcast: "Meta+Alt+KeyB",
      }
    : {
        newTab: "Control+Shift+T",
        close: "Control+Shift+W",
        splitRight: "Control+Shift+D",
        splitDown: "Control+Alt+Shift+D",
        focus: (arrow: string) => `Control+Alt+${arrow}`,
        localTerminal: "Control+Shift+L",
        zoom: "Control+Shift+Enter",
        broadcast: "Control+Alt+Shift+KeyB",
      };
}

/** The chords for the platform the specs pin (macOS; see the pinnedPlatform fixture). */
export const KEYS = workspaceKeys(true);

/** Makes the page report a Linux `navigator.platform` before any app code runs. */
export async function emulateLinux(page: Page) {
  await page.addInitScript(() => {
    Object.defineProperty(Navigator.prototype, "platform", { get: () => "Linux x86_64", configurable: true });
  });
}

/** iPhone 15 Pro in portrait, in CSS pixels (points). */
export const IPHONE_15_PRO = { width: 393, height: 852 };
/** iPad Air 11-inch (M4), in CSS pixels (points). */
export const IPAD_AIR_PORTRAIT = { width: 820, height: 1180 };
export const IPAD_AIR_LANDSCAPE = { width: 1180, height: 820 };
/** iPad software keyboard (with its shortcut strip) in portrait, in points. */
export const IPAD_KEYBOARD_PX = 400;
/** iPhone software keyboard (with its suggestion strip) in portrait, in points. */
export const IPHONE_KEYBOARD_PX = 336;

/**
 * Makes the page an iOS device of `size` before any app code runs: the backend's OS
 * (`window.__TETHER_OS__ = "ios"`, as the iOS build injects it), the viewport, and
 * `screen` (the form factor comes from its short side). Call before `page.goto`.
 */
export async function emulateIos(page: Page, size: { width: number; height: number } = IPHONE_15_PRO) {
  await page.setViewportSize(size);
  await page.addInitScript((s) => {
    window.__TETHER_OS__ = "ios";
    Object.defineProperty(Navigator.prototype, "platform", { get: () => "iPhone", configurable: true });
    Object.defineProperty(Screen.prototype, "width", { get: () => s.width, configurable: true });
    Object.defineProperty(Screen.prototype, "height", { get: () => s.height, configurable: true });
  }, size);
  await stubSoftKeyboard(page);
}

/** Pixel 9 in portrait, in CSS pixels. */
export const PIXEL_9 = { width: 412, height: 915 };
/** Pixel Tablet in landscape, in CSS pixels. */
export const PIXEL_TABLET = { width: 1280, height: 800 };

/**
 * Makes the page an Android device of `size` before any app code runs: the backend's OS
 * (`window.__TETHER_OS__ = "android"`, as `std::env::consts::OS` reports it), the viewport, and
 * `screen`. Call before `page.goto`.
 */
export async function emulateAndroid(page: Page, size: { width: number; height: number } = PIXEL_9) {
  await page.setViewportSize(size);
  await page.addInitScript((s) => {
    window.__TETHER_OS__ = "android";
    Object.defineProperty(Navigator.prototype, "platform", { get: () => "Linux armv81", configurable: true });
    Object.defineProperty(Screen.prototype, "width", { get: () => s.width, configurable: true });
    Object.defineProperty(Screen.prototype, "height", { get: () => s.height, configurable: true });
  }, size);
  await stubSoftKeyboard(page);
}

/** Lets `softKeyboard()` shrink the visual viewport (part of `emulateIos`; alone for other platforms). */
export async function stubSoftKeyboard(page: Page) {
  await page.addInitScript(() => {
    const height = Object.getOwnPropertyDescriptor(VisualViewport.prototype, "height")!.get!;
    Object.defineProperty(VisualViewport.prototype, "height", {
      get() {
        const covered = (window as { __softKeyboardPx?: number }).__softKeyboardPx ?? 0;
        return Math.max(0, (height.call(this) as number) - covered);
      },
      configurable: true,
    });
  });
}

/**
 * Shows (`px` > 0) or hides (0) a software keyboard of `px` points on a page set up
 * with `emulateIos` or `stubSoftKeyboard`: the visual viewport shrinks by it and reports a resize, as on iOS.
 */
export async function softKeyboard(page: Page, px: number) {
  await page.evaluate((covered) => {
    (window as { __softKeyboardPx?: number }).__softKeyboardPx = covered;
    window.visualViewport!.dispatchEvent(new Event("resize"));
  }, px);
}

/** Creates the vault through the UI, dismisses the recovery key dialog, and returns the key. */
export async function createVault(page: Page, password = PASSWORD): Promise<string> {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Create your vault" })).toBeVisible();
  await page.getByLabel("Master password", { exact: true }).fill(password);
  await page.getByLabel("Confirm master password").fill(password);
  await page.getByRole("button", { name: "Create vault" }).click();
  const dialog = page.getByRole("dialog", { name: "Your recovery key" });
  await expect(dialog).toBeVisible();
  const key = await dialog.getByLabel("Recovery key", { exact: true }).inputValue();
  await dialog.getByRole("button", { name: "I saved it" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  return key;
}

export const nav = (page: Page) => page.getByRole("navigation", { name: "Main" });

/**
 * Brings the main navigation on screen: as it is on a desktop; in the phone layout, back from a
 * terminal to the Hosts page and into the drawer (☰).
 */
export async function showNav(page: Page) {
  const back = page.getByRole("button", { name: "Back to hosts", exact: true });
  if (await back.isVisible()) await back.click();
  const menu = page.getByRole("button", { name: "Menu", exact: true });
  if (await menu.isVisible()) await menu.click();
}
/** The Hosts page's top bar: title, filter, Import SSH config…, New group, New host. */
export const hostsBar = (page: Page) => page.locator("header.hosts-top");
export const breadcrumb = (page: Page) => page.getByRole("navigation", { name: "Breadcrumb" });
/** The folder cards of the current group. */
export const folders = (page: Page) => page.getByRole("list", { name: "Groups" });
export const hostList = (page: Page) => page.getByRole("list", { name: "Hosts" });
export const hostForm = (page: Page) => page.getByRole("form", { name: "Host details" });

/** The list item holding a host's card. */
export const hostCard = (page: Page, label: string) =>
  hostList(page)
    .getByRole("listitem")
    .filter({ has: page.getByRole("button", { name: label, exact: true }) });

/** A host card's main button (clicking it connects). */
export function hostRow(page: Page, label: string): Locator {
  return hostList(page).getByRole("button", { name: label, exact: true });
}

/** Goes to a group from the top level by its path, e.g. "Production / Web" ("" stays at the top). */
export async function openGroupPath(page: Page, path: string) {
  await breadcrumb(page).getByRole("button", { name: "All hosts" }).click();
  for (const name of path.split(" / ").filter(Boolean)) {
    await folders(page).getByRole("button", { name, exact: true }).click();
  }
}

/** Opens a host in the editor through its card's ✎ (shown on hover). */
export async function editHost(page: Page, label: string) {
  await hostCard(page, label).hover();
  await hostList(page).getByRole("button", { name: `Edit host ${label}`, exact: true }).click();
}

export interface GroupOpts {
  /** Full path of the parent, e.g. "Production / Web"; default: the current group. */
  parent?: string;
  /** The group's own port. */
  port?: number;
}

export async function createGroup(page: Page, name: string, opts: GroupOpts = {}) {
  await hostsBar(page).getByRole("button", { name: "New group" }).click();
  const dialog = page.getByRole("dialog", { name: "New group" });
  await dialog.getByLabel("Name").fill(name);
  if (opts.parent) await dialog.getByLabel("Parent group").selectOption({ label: opts.parent });
  if (opts.port !== undefined) {
    await dialog.getByLabel("Port", { exact: true }).fill(String(opts.port));
  }
  await dialog.getByRole("button", { name: "Save group" }).click();
  await expect(dialog).toBeHidden();
}

export interface HostOpts {
  label: string;
  address: string;
  /** Full path of the group, e.g. "Production / Web". */
  group?: string;
  tags?: string[];
}

/** Creates a host through the editor, then closes the editor. */
export async function createHost(page: Page, opts: HostOpts) {
  await hostsBar(page).getByRole("button", { name: "New host" }).click();
  const form = hostForm(page);
  await form.getByLabel("Label").fill(opts.label);
  await form.getByLabel("Address").fill(opts.address);
  if (opts.group) await form.getByLabel("Group", { exact: true }).selectOption({ label: opts.group });
  for (const tag of opts.tags ?? []) {
    const existing = form.getByRole("checkbox", { name: tag, exact: true });
    if (await existing.count()) {
      await existing.check();
    } else {
      await form.getByLabel("New tag").fill(tag);
      await form.getByRole("button", { name: "Add tag" }).click();
    }
  }
  await form.getByRole("button", { name: "Save host" }).click();
  // Saving closes the editor.
  await expect(form).toBeHidden();
}

/**
 * Shows the Hosts page at its top level with no filter. The page remembers its group, filter and editor
 * between visits, so helpers that look for a host there start from a known place.
 */
export async function showHostsTop(page: Page) {
  await goTo(page, "Hosts");
  const filter = page.getByRole("searchbox", { name: "Filter hosts" });
  if ((await filter.inputValue()) !== "") await filter.fill("");
  await breadcrumb(page).getByRole("button", { name: "All hosts" }).click();
}

// -- the mock backend (VITE_TETHER_API=mock exposes it as window.__tetherMock) --

declare global {
  interface Window {
    __tetherMock: import("../src/api/mock").MockTetherApi;
    __tetherMockSeed?: import("../src/api/mock").MockSeed;
    /** A prompt answer a test is waiting for. */
    __answer?: Promise<import("../src/api/types").PromptResponse>;
    __answers?: Promise<import("../src/api/types").PromptResponse>[];
    /** Input elements a test keeps hold of, to check they were cleared. */
    __fields?: HTMLInputElement[];
  }
}

/** Ids of the live sessions, in opening order. */
export function sessionIds(page: Page): Promise<string[]> {
  return page.evaluate(async () => (await window.__tetherMock.activeWork()).sessions.map((s) => s.id));
}

/** Everything the UI wrote to a session, decoded as UTF-8. */
export function written(page: Page, sessionId: string): Promise<string> {
  return page.evaluate((id) => {
    const chunks = window.__tetherMock.control.written(id);
    return chunks.map((c) => new TextDecoder().decode(c)).join("");
  }, sessionId);
}

/**
 * Adds a host (at the top level) with an identity (so it can connect) through the mock, and
 * shows it on the hosts page, reloaded and back at its top level. Returns the host id.
 */
export async function seedHost(page: Page, label: string, address: string): Promise<string> {
  const id = await page.evaluate(
    async ({ label, address }) => {
      const api = window.__tetherMock;
      const identity = await api.upsertEntity("identity", { label: "deploy", username: "deploy", password: "pw" });
      const host = await api.upsertEntity("host", { label, address, settings: { identityId: identity.id } });
      return host.id;
    },
    { label, address },
  );
  // The Hosts page reloads when it shows again.
  await goTo(page, "Settings");
  await showHostsTop(page);
  await expect(hostRow(page, label)).toBeVisible();
  return id;
}

/**
 * Clicks the Connect action of a host card (shown on hover) at the Hosts page's top level, wherever
 * the page was left; in the phone layout (no Connect action) taps the card.
 */
export async function connectHost(page: Page, label: string) {
  await showHostsTop(page);
  await hostCard(page, label).hover();
  const connect = hostList(page).getByRole("button", { name: `Connect to ${label}`, exact: true });
  if (await connect.isVisible()) await connect.click();
  else await hostRow(page, label).click();
}

export const terminalTabs = (page: Page) => page.getByRole("tablist", { name: "Terminal tabs" }).getByRole("tab");
/** The visible tab's panel (in the phone layout, with no tab strip, the shown session's). */
export const terminalPanel = (page: Page) =>
  page.getByRole("tabpanel").or(page.locator(".layout-phone section.workspace:not([hidden]) .tab-panel:not([hidden])"));
/** The panes of the visible tab. */
export const panes = (page: Page) => terminalPanel(page).locator("[data-pane-id]");
export const activePane = (page: Page) => terminalPanel(page).locator('[data-pane-id][data-active="true"]');

/** Keeps hold of input elements (they outlive their dialog) to check their values later. */
export async function holdFields(...fields: Locator[]) {
  for (const [i, f] of fields.entries()) {
    await f.evaluate((el, first) => {
      if (first) window.__fields = [];
      window.__fields!.push(el as HTMLInputElement);
    }, i === 0);
  }
}

/** The current values of the held inputs. */
export function heldValues(page: Page): Promise<string[]> {
  return page.evaluate(() => (window.__fields ?? []).map((f) => f.value));
}

// -- keychain, known hosts, forwarding ----------------------------------------

export async function goTo(page: Page, section: string) {
  await showNav(page);
  await nav(page).getByRole("button", { name: section, exact: true }).click();
}

export const keyList = (page: Page) => page.getByRole("list", { name: "Keys" });
export const identityList = (page: Page) => page.getByRole("list", { name: "Identities" });
export const ruleList = (page: Page) => page.getByRole("list", { name: "Forwarding rules" });

/** The row of a key, identity or rule by its label. */
export const rowOf = (list: Locator, label: string) =>
  list.getByRole("listitem").filter({ has: list.page().getByText(label, { exact: true }) });

/** Generates an ed25519 key through the Keychain page and closes the result dialog. */
export async function generateKey(page: Page, label: string, passphrase?: string) {
  await goTo(page, "Keychain");
  await page.getByRole("button", { name: "Generate key" }).click();
  const dialog = page.getByRole("dialog", { name: "Generate key" });
  await dialog.getByLabel("Label").fill(label);
  if (passphrase) await dialog.getByLabel("Passphrase").fill(passphrase);
  await dialog.getByRole("button", { name: "Generate", exact: true }).click();
  await expect(dialog.getByRole("status")).toHaveText("Key generated");
  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(dialog).toBeHidden();
  await expect(rowOf(keyList(page), label)).toBeVisible();
}
