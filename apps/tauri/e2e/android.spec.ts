import { expect, test } from "./fixtures";
import type { Page } from "@playwright/test";
import {
  PIXEL_9,
  PIXEL_TABLET,
  activePane,
  connectHost,
  createVault,
  emulateAndroid,
  goTo,
  hostRow,
  nav,
  panes,
  seedHost,
  sessionIds,
} from "./helpers";

const menuButton = (page: Page) => page.getByRole("button", { name: "Menu", exact: true });
const drawer = (page: Page) => page.getByRole("dialog", { name: "Menu" });
const backButton = (page: Page) => page.getByRole("button", { name: "Back to hosts", exact: true });
const layoutButton = (page: Page) => page.getByRole("button", { name: "Layout", exact: true });
const keyBar = (page: Page) => page.getByRole("toolbar", { name: "Terminal keys" });
const sessionMenuButton = (page: Page) => page.getByRole("button", { name: "Session menu", exact: true });
const sessionMenu = (page: Page) => page.getByRole("menu", { name: "Session menu" });

/** What MainActivity does when the IME opens or closes: the CSS px it covers, then the event. */
async function nativeIme(page: Page, px: number) {
  await page.evaluate((v) => {
    (window as { __TETHER_IME_PX__?: number }).__TETHER_IME_PX__ = v;
    window.dispatchEvent(new Event("tether-ime"));
  }, px);
}

/** The terminal's last row ends at or above `y` (it refits, debounced). */
async function expectLastRowAbove(page: Page, y: number) {
  await expect
    .poll(async () => {
      const row = await activePane(page).locator(".xterm-rows > div").last().boundingBox();
      return row ? row.y + row.height <= y + 0.5 : false;
    })
    .toBe(true);
}

/** Taps web-1, waits for its shell and focuses the terminal. */
async function focusedTerminal(page: Page) {
  await hostRow(page, "web-1").click();
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await activePane(page).locator(".xterm").click();
  await expect(activePane(page).locator("textarea.xterm-helper-textarea")).toBeFocused();
}

/** Termlyx's folder with a folder and a file in it (the backend lists only that folder), then the SFTP page on web-1. */
async function sftpWithLocalFiles(page: Page) {
  await androidWithHost(page, PIXEL_9);
  await page.evaluate(() => {
    const entry = (name: string, isDir: boolean) => ({
      name,
      path: `/home/mock/${name}`,
      isDir,
      size: isDir ? 0 : 5,
      mtime: 0,
      mode: isDir ? 0o40755 : 0o100644,
      isSymlink: false,
    });
    window.__tetherMock.control.addLocal(entry("Projects", true));
    window.__tetherMock.control.addLocal(entry("notes.txt", false));
  });
  await goTo(page, "SFTP");
  await page.getByLabel("Host", { exact: true }).selectOption({ label: "web-1" });
}

/** The system Back button or gesture (the mock plays the part of the app's Back listener). */
const pressBack = (page: Page) => page.evaluate(() => window.__tetherMock.control.backButton());
const backgrounded = (page: Page) => page.evaluate(() => window.__tetherMock.control.backgrounded());

/** A vault with host web-1 on an Android device of `size`. */
async function androidWithHost(page: Page, size: { width: number; height: number }) {
  await emulateAndroid(page, size);
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
}

test.describe("Pixel 9", () => {
  test("hosts_start_page_and_menu_drawer", async ({ page }) => {
    await androidWithHost(page, PIXEL_9);
    await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
    await expect(page.locator(".layout-phone")).toHaveCount(1);
    await expect(nav(page)).toHaveCount(0);
    await menuButton(page).click();
    await expect(drawer(page)).toBeVisible();
    await expect(drawer(page).getByRole("button", { name: "SFTP", exact: true })).toBeVisible();
    await expect(drawer(page).locator("kbd")).toHaveCount(0);
  });

  test("tapping_a_host_opens_a_full_screen_terminal_with_back_to_hosts", async ({ page }) => {
    await androidWithHost(page, PIXEL_9);
    await hostRow(page, "web-1").click();
    await expect(activePane(page)).toHaveAttribute("data-status", "open");
    await expect(backButton(page)).toBeVisible();
    await expect(page.getByRole("tablist")).toHaveCount(0);
    expect(await sessionIds(page)).toHaveLength(1);
    // Touch scrolling is on for any mobile platform.
    await expect(activePane(page).locator("[data-touch-scroll]")).toHaveCount(1);
    await backButton(page).click();
    await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  });

  test("the_new_tab_picker_has_no_local_terminal", async ({ page }) => {
    await androidWithHost(page, PIXEL_9);
    await hostRow(page, "web-1").click();
    await expect(activePane(page)).toHaveAttribute("data-status", "open");
    await page.keyboard.press("Control+Shift+T");
    await expect(page.getByRole("heading", { name: "New tab" })).toBeVisible();
    await expect(page.getByRole("list", { name: "Connect to a host" })).toBeVisible();
    await expect(page.getByText("Local terminal")).toHaveCount(0);
    await backButton(page).click();
    await expect(page.getByText("Local terminal")).toHaveCount(0);
  });

  test("words_say_this_phone", async ({ page }) => {
    await androidWithHost(page, PIXEL_9);
    await goTo(page, "SFTP");
    await page.getByLabel("Host", { exact: true }).selectOption({ label: "web-1" });
    const local = page.getByRole("region", { name: "Local files" });
    await expect(local.getByRole("heading", { name: "This phone › Termlyx" })).toBeVisible();
    // Android imports through its own button, not iOS's Files.
    await expect(local.getByRole("button", { name: "Import from Files", exact: true })).toHaveCount(0);
    await expect(local.getByRole("button", { name: "Import from device", exact: true })).toBeVisible();
  });

  test("import_from_device_lists_the_picked_files_and_numbers_a_clash", async ({ page }) => {
    await sftpWithLocalFiles(page);
    const local = page.getByRole("region", { name: "Local files" });
    const names = local.getByRole("rowheader");
    const importButton = local.getByRole("button", { name: "Import from device", exact: true });
    const pick = (picked: string[]) => page.evaluate((p) => window.__tetherMock.control.setDevicePick(p), picked);
    await expect(names).toHaveText(["Projects", "notes.txt"]);

    // A cancelled picker changes nothing.
    await importButton.click();
    await expect(names).toHaveText(["Projects", "notes.txt"]);
    await expect(page.getByRole("alert")).toHaveCount(0);

    // Several files at once; notes.txt is taken, so the copy is numbered.
    await pick(["IMG_0042.jpg", "notes.txt"]);
    await importButton.click();
    await expect(page.getByRole("status").filter({ hasText: "Imported 2 files." })).toBeVisible();
    await expect(names).toHaveText(["Projects", "IMG_0042.jpg", "notes (2).txt", "notes.txt"]);
  });

  test("save_to_device_is_for_one_local_file", async ({ page }) => {
    await sftpWithLocalFiles(page);
    const local = page.getByRole("region", { name: "Local files" });
    const save = local.getByRole("button", { name: "Save to device…", exact: true });
    const saved = () => page.evaluate(() => window.__tetherMock.control.savedToDevice());

    // Nothing selected, then a folder: no.
    await expect(save).toBeDisabled();
    await local.getByRole("rowheader", { name: "Projects" }).click();
    await expect(save).toBeDisabled();

    // A cancelled "save as" says nothing.
    await local.getByRole("rowheader", { name: "notes.txt" }).click();
    await expect(save).toBeEnabled();
    await page.evaluate(() => window.__tetherMock.control.cancelNextSave());
    await save.click();
    await expect.poll(saved).toEqual([]);
    await expect(page.getByRole("status").filter({ hasText: "Saved" })).toHaveCount(0);

    await save.click();
    await expect(page.getByRole("status").filter({ hasText: "Saved notes.txt to your phone." })).toBeVisible();
    expect(await saved()).toEqual(["/home/mock/notes.txt"]);
  });

  // A second picker would take over the first one's pending result, and the first call would never finish.
  test("device_buttons_wait_for_an_open_picker", async ({ page }) => {
    await sftpWithLocalFiles(page);
    const local = page.getByRole("region", { name: "Local files" });
    const importButton = local.getByRole("button", { name: "Import from device", exact: true });
    const save = local.getByRole("button", { name: "Save to device…", exact: true });
    await local.getByRole("rowheader", { name: "notes.txt" }).click();
    await expect(save).toBeEnabled();

    await page.evaluate(() => {
      (window as unknown as { __held: { release(): void } }).__held = window.__tetherMock.control.holdNextDevicePick();
    });
    await importButton.click();
    await expect(importButton).toBeDisabled();
    await expect(save).toBeDisabled();

    await page.evaluate(() => (window as unknown as { __held: { release(): void } }).__held.release());
    await expect(importButton).toBeEnabled();
    await expect(save).toBeEnabled();
  });
});

test.describe("Pixel 9 system Back", () => {
  test("steps_back_one_level_at_a_time_and_ends_in_the_background_with_the_session_open", async ({ page }) => {
    await androidWithHost(page, PIXEL_9);
    await hostRow(page, "web-1").click();
    await expect(activePane(page)).toHaveAttribute("data-status", "open");

    // A dialog first.
    await sessionMenuButton(page).click();
    await sessionMenu(page).getByRole("menuitem", { name: "Disconnect all" }).click();
    const confirm = page.getByRole("dialog", { name: "Disconnect all" });
    await expect(confirm).toBeVisible();
    await pressBack(page);
    await expect(confirm).toHaveCount(0);
    await expect(backButton(page)).toBeVisible();

    // Then the docked panel.
    await sessionMenuButton(page).click();
    await sessionMenu(page).getByRole("menuitem", { name: "Snippets" }).click();
    const sheet = page.getByRole("complementary", { name: "Snippets" });
    await expect(sheet).toBeVisible();
    await pressBack(page);
    await expect(sheet).toHaveCount(0);
    await expect(backButton(page)).toBeVisible();

    // Then the terminal to Hosts.
    await pressBack(page);
    await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
    expect(await backgrounded(page)).toBe(0);

    // The drawer is not a dialog: Back closes it, and does not leave the page.
    await menuButton(page).click();
    await expect(drawer(page)).toBeVisible();
    await pressBack(page);
    await expect(drawer(page)).toHaveCount(0);
    expect(await backgrounded(page)).toBe(0);

    // Hosts with nothing open: the background, the session still there.
    await pressBack(page);
    await expect.poll(() => backgrounded(page)).toBe(1);
    expect(await sessionIds(page)).toHaveLength(1);
    await menuButton(page).click();
    await expect(drawer(page).getByRole("button", { name: "Terminals (1)" })).toBeVisible();
  });

  test("a_popup_menu_closes_before_the_app_goes_to_the_background", async ({ page }) => {
    await androidWithHost(page, PIXEL_9);
    await hostRow(page, "web-1").click({ button: "right" });
    const menu = page.getByRole("menu", { name: "web-1" });
    await expect(menu).toBeVisible();
    await pressBack(page);
    await expect(menu).toHaveCount(0);
    expect(await backgrounded(page)).toBe(0);
  });

  test("a_page_goes_to_hosts_then_the_background", async ({ page }) => {
    await androidWithHost(page, PIXEL_9);
    await goTo(page, "SFTP");
    await pressBack(page);
    await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
    expect(await backgrounded(page)).toBe(0);
    await pressBack(page);
    await expect.poll(() => backgrounded(page)).toBe(1);
  });
});

test.describe("Pixel 9 keyboard", () => {
  test("key_bar_shows_over_the_native_ime_and_the_last_row_stays_above_it", async ({ page }) => {
    await androidWithHost(page, PIXEL_9);
    await focusedTerminal(page);
    await expect(keyBar(page)).toHaveCount(0);
    // MainActivity's gesture bar inset: cleared without the IME, under it with one (the IME's height includes it).
    await page.evaluate(() => document.documentElement.style.setProperty("--inset-bottom", "24px"));
    await expect(page.locator(".app")).toHaveCSS("padding-bottom", "24px");
    await nativeIme(page, 300);
    await expect(keyBar(page)).toBeVisible();
    const bar = (await keyBar(page).boundingBox())!;
    // The bar sits on the IME.
    expect(bar.y + bar.height).toBeLessThanOrEqual(PIXEL_9.height - 300 + 0.5);
    expect(bar.y + bar.height).toBeGreaterThan(PIXEL_9.height - 300 - 2);
    await expectLastRowAbove(page, bar.y);
    await nativeIme(page, 0);
    await expect(keyBar(page)).toHaveCount(0);
  });

  test("keyboard_then_panel_then_rotate_keeps_last_row_visible", async ({ page }) => {
    await androidWithHost(page, PIXEL_9);
    await focusedTerminal(page);
    await nativeIme(page, 300);
    await expect(keyBar(page)).toBeVisible();
    await sessionMenuButton(page).click();
    await sessionMenu(page).getByRole("menuitem", { name: "Snippets" }).click();
    const sheet = page.getByRole("complementary", { name: "Snippets" });
    await expect(sheet).toBeVisible();
    // The panel took the terminal's focus, so the IME went down.
    await nativeIme(page, 0);
    await page.setViewportSize({ width: PIXEL_9.height, height: PIXEL_9.width });
    await expect(sheet).toBeVisible();
    const box = (await sheet.boundingBox())!;
    expect(box.y + box.height).toBeLessThanOrEqual(PIXEL_9.width + 0.5);
    await expectLastRowAbove(page, box.y);
  });

  test("a_long_press_shows_no_webview_menu_or_selection", async ({ page }) => {
    await androidWithHost(page, PIXEL_9);
    await focusedTerminal(page);
    // Only Termlyx's own menu (TouchSelection): the WebView's context menu and text selection are cancelled.
    const cancelled = await activePane(page)
      .locator(".xterm-screen")
      .evaluate((el) =>
        ["contextmenu", "selectstart"].map((type) => !el.dispatchEvent(new Event(type, { bubbles: true, cancelable: true }))),
      );
    expect(cancelled).toEqual([true, true]);
  });
});

test.describe("Pixel Tablet", () => {
  test("rail_and_layout_button_and_ctrl_labels", async ({ page }) => {
    await androidWithHost(page, PIXEL_TABLET);
    await expect(page.locator(".layout-phone")).toHaveCount(0);
    await expect(page.locator("aside.sidebar")).toHaveAttribute("data-mode", "rail");
    // The Connect button, as a mouse reaches it: the card's hover actions cover its middle at this width.
    await connectHost(page, "web-1");
    await expect(activePane(page)).toHaveAttribute("data-status", "open");
    await expect(layoutButton(page)).toBeVisible();
    await expect(page.getByRole("button", { name: "New tab", exact: true })).toHaveAttribute("title", "New tab (Ctrl+Shift+T)");
    await expect(page.locator("body")).not.toContainText("⌘");
  });

  test("full_sidebar_search_has_no_shortcut_hint", async ({ page }) => {
    await androidWithHost(page, PIXEL_TABLET);
    await page.getByRole("button", { name: "Expand sidebar", exact: true }).click();
    await expect(page.locator("aside.sidebar")).toHaveAttribute("data-mode", "full");
    await expect(page.locator("aside.sidebar .search-button kbd")).toHaveCount(0);
  });

  test("split_works_by_menu_and_by_chord", async ({ page }) => {
    await androidWithHost(page, PIXEL_TABLET);
    // The Connect button, as a mouse reaches it: the card's hover actions cover its middle at this width.
    await connectHost(page, "web-1");
    await expect(activePane(page)).toHaveAttribute("data-status", "open");
    await layoutButton(page).click();
    await page.getByRole("menuitem", { name: /^Split right(?!…)/ }).click();
    await expect(panes(page)).toHaveCount(2);
    await expect.poll(() => sessionIds(page)).toHaveLength(2);
    await activePane(page).locator(".xterm").click();
    await page.keyboard.press("Control+Shift+D");
    await expect(panes(page)).toHaveCount(3);
  });
});
