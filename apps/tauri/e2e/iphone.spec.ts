import { expect, test } from "./fixtures";
import type { Locator, Page } from "@playwright/test";
import {
  IPHONE_KEYBOARD_PX,
  KEYS,
  PASSWORD,
  activePane,
  createVault,
  emulateIos,
  goTo,
  hostRow,
  nav,
  seedHost,
  sessionIds,
  softKeyboard,
  terminalPanel,
} from "./helpers";

// iPhone 15 Pro, portrait, iOS (the backend's OS), before the app loads.
test.beforeEach(async ({ page }) => {
  await emulateIos(page);
});

const menuButton = (page: Page) => page.getByRole("button", { name: "Menu", exact: true });
const drawer = (page: Page) => page.getByRole("dialog", { name: "Menu" });
const backButton = (page: Page) => page.getByRole("button", { name: "Back to hosts", exact: true });
const sessionMenuButton = (page: Page) => page.getByRole("button", { name: "Session menu", exact: true });
const sessionMenu = (page: Page) => page.getByRole("menu", { name: "Session menu" });
const card = (page: Page) => terminalPanel(page).locator(".session-card");
const keyBar = (page: Page) => page.getByRole("toolbar", { name: "Terminal keys" });
const NAV_ITEMS = ["Hosts", "Keychain", "Port Forwarding", "Known Hosts", "SFTP", "Settings", "Terminals"];

/** A vault with host web-1, on the Hosts page. */
async function withHost(page: Page) {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
}

/** Taps web-1 and waits for its shell. */
async function openWeb1(page: Page) {
  await hostRow(page, "web-1").click();
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
}

/** The element's box lies inside the visual viewport (the part above the software keyboard). */
async function expectInVisibleArea(page: Page, el: Locator) {
  const box = (await el.boundingBox())!;
  const visible = await page.evaluate(() => ({ w: window.visualViewport!.width, h: window.visualViewport!.height }));
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(visible.w + 0.5);
  expect(box.y + box.height).toBeLessThanOrEqual(visible.h + 0.5);
}

test("after_unlock_the_hosts_page_shows_with_the_menu_button_and_no_sidebar", async ({ page }) => {
  await createVault(page);
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  await expect(menuButton(page)).toBeVisible();
  await expect(page.getByRole("button", { name: "New host", exact: true })).toBeVisible();
  await expect(page.getByRole("searchbox", { name: "Filter hosts" })).toBeVisible();
  // The sidebar only lives in the drawer.
  await expect(nav(page)).toHaveCount(0);
  await expect(page.locator(".sidebar")).toHaveCount(0);
  // ＋ opens the new-host form.
  await page.getByRole("button", { name: "New host", exact: true }).click();
  await expect(page.getByRole("form", { name: "Host details" })).toBeVisible();
});

test("the_menu_button_opens_the_drawer_with_every_page", async ({ page }) => {
  await createVault(page);
  await menuButton(page).click();
  await expect(drawer(page)).toBeVisible();
  const main = drawer(page).getByRole("navigation", { name: "Main" });
  for (const item of NAV_ITEMS) await expect(main.getByRole("button", { name: item, exact: true })).toBeVisible();
  for (const item of ["Search", "Lock vault"]) {
    await expect(drawer(page).getByRole("button", { name: item, exact: true })).toBeVisible();
  }
  // No desktop-only controls: the rail toggle, shortcut hints.
  await expect(drawer(page).getByRole("button", { name: "Collapse sidebar" })).toHaveCount(0);
  await expect(drawer(page).locator("kbd")).toHaveCount(0);

  // Navigating closes it and shows the page under the top bar's name.
  await main.getByRole("button", { name: "Keychain", exact: true }).click();
  await expect(drawer(page)).toBeHidden();
  await expect(page.getByRole("heading", { name: "Keychain", level: 1 })).toBeAttached();
  await expect(page.locator(".phone-top-title")).toHaveText("Keychain");
  await expect(page.getByRole("button", { name: "New host", exact: true })).toHaveCount(0);

  // A tap outside the panel closes it; so does Escape.
  await menuButton(page).click();
  await expect(drawer(page)).toBeVisible();
  await page.mouse.click(380, 400);
  await expect(drawer(page)).toBeHidden();
  await menuButton(page).click();
  await expect(drawer(page)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(drawer(page)).toBeHidden();
  await expect(menuButton(page)).toBeFocused();
});

test("a_swipe_from_the_left_edge_opens_the_drawer", async ({ page }) => {
  await createVault(page);
  await page.evaluate(() => {
    const at = (x: number) => [new Touch({ identifier: 1, target: document.body, clientX: x, clientY: 400 })];
    const send = (type: string, x: number) =>
      window.dispatchEvent(new TouchEvent(type, { touches: type === "touchend" ? [] : at(x), changedTouches: at(x) }));
    send("touchstart", 8);
    send("touchmove", 40);
    send("touchmove", 120);
    send("touchend", 120);
  });
  await expect(drawer(page)).toBeVisible();
});

test("tapping_a_host_opens_a_full_screen_terminal_with_the_connect_steps", async ({ page }) => {
  await withHost(page);
  await page.evaluate(() => {
    (window as unknown as { __held: { release(): void } }).__held = window.__tetherMock.control.holdNextOpenSession();
  });
  await hostRow(page, "web-1").click();

  // The top bar: back, the host's label, the session menu; no tab strip.
  await expect(backButton(page)).toBeVisible();
  await expect(page.locator(".phone-top-title")).toHaveText("web-1");
  await expect(sessionMenuButton(page)).toBeVisible();
  await expect(page.getByRole("tablist")).toHaveCount(0);

  // The session card shows the steps.
  await expect(card(page).getByRole("status")).toContainText("Connecting…");
  await expect(card(page).getByRole("listitem").first()).toHaveAttribute("data-mark", "current");

  await page.evaluate(() => (window as unknown as { __held: { release(): void } }).__held.release());
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await expect(card(page)).toHaveCount(0);

  // The terminal fills the width of the screen, under the top bar.
  const pane = (await activePane(page).boundingBox())!;
  const bar = (await page.locator(".phone-top").boundingBox())!;
  expect(pane.width).toBeGreaterThanOrEqual(393 - 2);
  expect(pane.y).toBeGreaterThanOrEqual(bar.y + bar.height - 1);
});

test("back_to_hosts_keeps_the_session_and_terminals_lists_it", async ({ page }) => {
  await withHost(page);
  await openWeb1(page);
  const [id] = await sessionIds(page);

  await backButton(page).click();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  await expect(menuButton(page)).toBeVisible();
  expect(await sessionIds(page)).toEqual([id]);

  await menuButton(page).click();
  await drawer(page).getByRole("button", { name: "Terminals (1)" }).click();
  await expect(drawer(page)).toBeHidden();
  const list = page.getByRole("list", { name: "Open sessions" });
  await expect(list.getByRole("listitem")).toHaveCount(1);
  await expect(list.getByRole("button", { name: "web-1, Connected" })).toBeVisible();

  // A tap shows that session again: the same one.
  await list.getByRole("button", { name: "web-1, Connected" }).click();
  await expect(backButton(page)).toBeVisible();
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  expect(await sessionIds(page)).toEqual([id]);
});

test("a_swipe_to_the_left_in_the_session_list_reveals_close", async ({ page }) => {
  await withHost(page);
  await openWeb1(page);
  await backButton(page).click();
  await menuButton(page).click();
  await drawer(page).getByRole("button", { name: "Terminals (1)" }).click();
  const row = page.locator(".phone-session").first();
  await expect(row).not.toHaveAttribute("data-revealed", "true");

  const main = row.locator(".phone-session-main");
  await main.evaluate((el) => {
    const box = el.getBoundingClientRect();
    const y = box.top + box.height / 2;
    const at = (x: number) => [new Touch({ identifier: 1, target: el, clientX: x, clientY: y })];
    const send = (type: string, x: number) =>
      el.dispatchEvent(new TouchEvent(type, { bubbles: true, touches: type === "touchend" ? [] : at(x), changedTouches: at(x) }));
    send("touchstart", 300);
    send("touchmove", 240);
    send("touchend", 240);
  });
  await expect(row).toHaveAttribute("data-revealed", "true");
  // The label stays in view beside Close.
  const close = row.getByRole("button", { name: "Close session web-1" });
  const closeX = (await close.boundingBox())!.x;
  const rowX = (await row.boundingBox())!.x;
  await expect
    .poll(async () => {
      const label = (await row.locator(".phone-session-label").boundingBox())!;
      return label.x >= rowX && label.x + label.width <= closeX;
    })
    .toBe(true);
  await close.click();
  await expect.poll(() => sessionIds(page)).toHaveLength(0);
  // The last session gone: back to a page.
  await expect(menuButton(page)).toBeVisible();
});

test("the_session_menu_closes_reconnects_and_opens_snippets", async ({ page }) => {
  await withHost(page);
  await openWeb1(page);
  const [first] = await sessionIds(page);

  await sessionMenuButton(page).click();
  await expect(sessionMenu(page).getByRole("menuitem")).toHaveText([
    "Close session",
    "Reconnect",
    "Paste",
    "Snippets",
    "Themes",
    "Disconnect all",
  ]);

  // Snippets: the key bar's panel, docked under the terminal.
  await sessionMenu(page).getByRole("menuitem", { name: "Snippets" }).click();
  const sheet = page.getByRole("complementary", { name: "Snippets" });
  await expect(sheet).toContainText("No snippets yet.");
  // Full width, at the bottom.
  const box = (await sheet.boundingBox())!;
  expect(box.width).toBeGreaterThanOrEqual(393 - 1);
  expect(box.y + box.height).toBeGreaterThanOrEqual(852 - 1);
  await sheet.getByRole("button", { name: "Close" }).click();
  await expect(sheet).toBeHidden();

  // Reconnect: a new session in its place.
  await sessionMenuButton(page).click();
  await sessionMenu(page).getByRole("menuitem", { name: "Reconnect" }).click();
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await expect.poll(async () => (await sessionIds(page))[0]).not.toBe(first);
  await expect(activePane(page)).toHaveAttribute("data-status", "open");

  // Close session: back to the hosts.
  await sessionMenuButton(page).click();
  await sessionMenu(page).getByRole("menuitem", { name: "Close session" }).click();
  await expect.poll(() => sessionIds(page)).toHaveLength(0);
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
});

test("disconnect_all_asks_then_closes_every_session", async ({ page }) => {
  await withHost(page);
  await seedHost(page, "db-1", "db1.example");
  await openWeb1(page);
  await backButton(page).click();
  await hostRow(page, "db-1").click();
  await expect.poll(() => sessionIds(page)).toHaveLength(2);

  await sessionMenuButton(page).click();
  await sessionMenu(page).getByRole("menuitem", { name: "Disconnect all" }).click();
  const confirm = page.getByRole("dialog", { name: "Disconnect all" });
  await expect(confirm).toContainText("all 2 open sessions");
  await confirm.getByRole("button", { name: "Disconnect all" }).click();
  await expect.poll(() => sessionIds(page)).toHaveLength(0);
  await expect(menuButton(page)).toBeVisible();
});

test("no_splits_and_no_local_terminal", async ({ page }) => {
  await withHost(page);
  await openWeb1(page);
  await expect(page.getByText("Local terminal")).toHaveCount(0);

  // The split chords do nothing: one pane, no divider.
  await activePane(page).locator(".xterm").click();
  await page.keyboard.press(KEYS.splitRight);
  await page.keyboard.press(KEYS.splitDown);
  await expect(terminalPanel(page).locator("[data-pane-id]")).toHaveCount(1);
  await expect(page.locator(".split-handle")).toHaveCount(0);
  expect(await sessionIds(page)).toHaveLength(1);
  // Nor does the session menu offer them.
  await sessionMenuButton(page).click();
  await expect(sessionMenu(page).getByRole("menuitem", { name: /split/i })).toHaveCount(0);
  await page.keyboard.press("Escape");

  // A new tab's picker (a hardware keyboard's chord) offers hosts only.
  await page.keyboard.press(KEYS.newTab);
  await expect(page.getByRole("heading", { name: "New tab" })).toBeVisible();
  await expect(page.getByRole("list", { name: "Connect to a host" })).toBeVisible();
  await expect(page.getByText("Local terminal")).toHaveCount(0);

  // Nor anywhere in the drawer or on the Hosts page.
  await backButton(page).click();
  await expect(page.getByText("Local terminal")).toHaveCount(0);
  await menuButton(page).click();
  await expect(drawer(page).getByText("Local terminal")).toHaveCount(0);
});

test("iphone_has_no_split_ui", async ({ page }) => {
  await withHost(page);
  await openWeb1(page);
  await expect(page.getByRole("button", { name: "Layout", exact: true })).toHaveCount(0);
  await activePane(page).locator(".xterm").click();
  await page.keyboard.press(KEYS.splitRight);
  await expect(terminalPanel(page).locator("[data-pane-id]")).toHaveCount(1);
  expect(await sessionIds(page)).toHaveLength(1);
  // The palette (a hardware keyboard's) offers no split.
  await page.keyboard.press("Meta+k");
  const palette = page.getByRole("dialog", { name: "Search" });
  await expect(palette).toBeVisible();
  await palette.getByRole("combobox", { name: "Search" }).fill("split");
  await expect(palette.getByRole("option", { name: /^Split/ })).toHaveCount(0);
});

test.describe("the session card with the software keyboard up (review focus 5)", () => {
  test("a_connecting_card_stays_above_the_key_bar_and_keyboard", async ({ page }) => {
    await withHost(page);
    await page.evaluate(() => window.__tetherMock.control.holdNextOpenSession());
    await hostRow(page, "web-1").click();
    await expect(card(page).getByRole("status")).toContainText("Connecting…");
    // The terminal under the card has the focus (the keyboard is for it), so the key bar comes up too.
    await expect(activePane(page).locator("textarea.xterm-helper-textarea")).toBeFocused();
    await softKeyboard(page, IPHONE_KEYBOARD_PX);
    await expect(keyBar(page)).toBeVisible();

    const bar = (await keyBar(page).boundingBox())!;
    const box = (await card(page).boundingBox())!;
    expect(box.y + box.height).toBeLessThanOrEqual(bar.y + 0.5);
    await expectInVisibleArea(page, card(page));
    const cancel = card(page).getByRole("button", { name: "Cancel" });
    await expectInVisibleArea(page, cancel);
    const b = (await cancel.boundingBox())!;
    expect(b.y + b.height).toBeLessThanOrEqual(bar.y + 0.5);
    await cancel.click();
    await expect.poll(() => sessionIds(page)).toHaveLength(0);
  });

  test("a_failed_card_keeps_its_buttons_in_reach", async ({ page }) => {
    await withHost(page);
    await page.evaluate(() => window.__tetherMock.control.failNextOpenSession("connection refused (os error 111)"));
    await hostRow(page, "web-1").click();
    await expect(card(page).getByRole("alert")).toContainText("Could not connect");
    await softKeyboard(page, IPHONE_KEYBOARD_PX);
    for (const name of ["Retry", "Close"]) await expectInVisibleArea(page, card(page).getByRole("button", { name }));
    await expectInVisibleArea(page, card(page));
  });

  test("in_landscape_the_card_scrolls_inside_and_its_buttons_are_reachable", async ({ page }) => {
    await page.setViewportSize({ width: 852, height: 393 });
    await withHost(page);
    await page.evaluate(() => window.__tetherMock.control.holdNextOpenSession());
    await hostRow(page, "web-1").click();
    await expect(card(page).getByRole("status")).toContainText("Connecting…");
    await softKeyboard(page, 200);
    await expect(keyBar(page)).toBeVisible();
    // The card fits the space left; what does not fit scrolls inside it.
    await expectInVisibleArea(page, card(page));
    const bar = (await keyBar(page).boundingBox())!;
    const box = (await card(page).boundingBox())!;
    expect(box.y + box.height).toBeLessThanOrEqual(bar.y + 0.5);
    const cancel = card(page).getByRole("button", { name: "Cancel" });
    await cancel.scrollIntoViewIfNeeded();
    await expectInVisibleArea(page, cancel);
    const b = (await cancel.boundingBox())!;
    expect(b.y + b.height).toBeLessThanOrEqual(bar.y + 0.5);
    await cancel.click();
    await expect.poll(() => sessionIds(page)).toHaveLength(0);
  });
});

test("face_id_quick_unlock_is_named_and_the_master_password_stays_the_fallback", async ({ page }) => {
  // A locked vault with Face ID unlock on, and the first Face ID declined.
  await page.addInitScript((password) => {
    window.__tetherMockSeed = {
      quickUnlockKind: "faceId",
      lockedVault: { password, quickUnlock: true },
      quickUnlockFailure: { message: "Face ID was not confirmed", code: "presenceDenied" },
    };
  }, PASSWORD);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Unlock with Face ID" })).toBeVisible();
  // The first unlock screen asks Face ID by itself, once; declining is not an error.
  await expect.poll(() => page.evaluate(() => window.__tetherMock.control.quickUnlockCalls())).toBe(1);
  await expect(page.getByRole("alert")).toHaveCount(0);

  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await goTo(page, "Settings");
  await expect(page.getByRole("checkbox", { name: "Unlock with Face ID" })).toBeChecked();
  await expect(
    page.getByText(
      "Keeps a key on this iPhone that Termlyx only uses after Face ID. Changing enrolled faces turns it off. The master password still works.",
    ),
  ).toBeVisible();

  // Locked from the menu: the button unlocks with Face ID.
  await menuButton(page).click();
  await drawer(page).getByRole("button", { name: "Lock vault", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
  await page.getByRole("button", { name: "Unlock with Face ID" }).click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeVisible();
  expect(await page.evaluate(() => window.__tetherMock.control.quickUnlockCalls())).toBe(2);
});

test.describe("SFTP local pane is Termlyx's folder in Files", () => {
  const ROOT = "/home/mock";
  const localPane = (page: Page) => page.getByRole("region", { name: "Local files" });
  const pathBox = (page: Page) => localPane(page).getByRole("textbox", { name: "Path", exact: true });
  const up = (page: Page) => localPane(page).getByRole("button", { name: "Up", exact: true });

  async function openSftp(page: Page) {
    // The backend confines the pane to one folder and lists only it as a drive; two shells would show a picker elsewhere.
    await page.addInitScript((root) => {
      window.__tetherMockSeed = {
        localDrives: [root],
        localShells: [
          { id: "a", label: "A" },
          { id: "b", label: "B" },
        ],
      };
    }, ROOT);
    await withHost(page);
    await page.evaluate((root) => {
      const control = window.__tetherMock.control;
      const entry = (path: string, isDir: boolean) => ({
        name: path.slice(path.lastIndexOf("/") + 1),
        path,
        isDir,
        size: isDir ? 0 : 5,
        mtime: 0,
        mode: isDir ? 0o40755 : 0o100644,
        isSymlink: false,
      });
      control.addLocal(entry(`${root}/Projects`, true));
      control.addLocal(entry(`${root}/notes.txt`, false));
    }, ROOT);
    await goTo(page, "SFTP");
    await page.getByLabel("Host", { exact: true }).selectOption({ label: "web-1" });
    await expect(localPane(page)).toBeVisible();
  }

  test("the_pane_is_named_after_the_device_and_cannot_go_above_the_folder", async ({ page }) => {
    await openSftp(page);
    await expect(localPane(page).getByRole("heading", { name: "On My iPhone › Termlyx" })).toBeVisible();
    await expect(pathBox(page)).toHaveValue("/");
    await expect(localPane(page).getByRole("rowheader")).toHaveText(["Projects", "notes.txt"]);
    await expect(up(page)).toBeDisabled();

    // Below the folder Up works, and stops again at the top.
    await localPane(page).getByRole("rowheader", { name: "Projects" }).dblclick();
    await expect(pathBox(page)).toHaveValue("/Projects");
    await up(page).click();
    await expect(pathBox(page)).toHaveValue("/");
    await expect(up(page)).toBeDisabled();
    // No This PC list (Windows only).
    await expect(localPane(page).getByRole("button", { name: "This PC" })).toHaveCount(0);
  });

  test("import_from_files_moves_the_picked_file_in_and_numbers_a_clash", async ({ page }) => {
    await openSftp(page);
    const importButton = localPane(page).getByRole("button", { name: "Import from Files", exact: true });
    // The Android picker buttons are not iOS's.
    await expect(localPane(page).getByRole("button", { name: /device/ })).toHaveCount(0);
    const pick = (path: string | null) =>
      page.evaluate((p) => {
        const control = window.__tetherMock.control;
        if (p !== null) control.setFile(decodeURIComponent(p.replace(/^file:\/\//, "")), "hello");
        control.setPick(p);
      }, path);

    // Cancelling the picker changes nothing.
    await pick(null);
    await importButton.click();
    await expect(localPane(page).getByRole("rowheader")).toHaveText(["Projects", "notes.txt"]);

    // A file:// URL, as the iOS picker returns.
    await pick("file:///tmp/Inbox/notes.txt");
    await importButton.click();
    await expect(page.getByRole("status").filter({ hasText: "Imported notes 2.txt into Termlyx's folder." })).toBeVisible();
    await expect(localPane(page).getByRole("rowheader")).toHaveText(["Projects", "notes 2.txt", "notes.txt"]);

    await pick("file:///tmp/Inbox/report%20final.pdf");
    await importButton.click();
    await expect(localPane(page).getByRole("rowheader", { name: "report final.pdf" })).toBeVisible();

    // Something the picker did not hand over is refused with a message.
    await page.evaluate(() => window.__tetherMock.control.setPick("file:///etc/passwd"));
    await importButton.click();
    await expect(page.getByRole("alert").filter({ hasText: "not a file the picker handed over" })).toBeVisible();
  });

  test.describe("on a touch screen", () => {
    // Coarse pointer: buttons grow to 44 pt, as on the device.
    test.use({ hasTouch: true, isMobile: true });

    test("both_listings_have_room_on_a_phone", async ({ page }) => {
      // Task 20 simulator pass: stacked in one column, the host's listing was squeezed to a sliver.
      await openSftp(page);
      for (const name of ["Local files", "Remote files"]) {
        const listing = page.getByRole("region", { name }).locator(".pane-scroll");
        await listing.scrollIntoViewIfNeeded();
        await expect(listing, name).toBeInViewport();
        const box = await listing.boundingBox();
        expect(box?.height ?? 0, name).toBeGreaterThanOrEqual(150);
      }
    });
  });

  test("settings_offer_no_local_shell", async ({ page }) => {
    await openSftp(page);
    await goTo(page, "Settings");
    await expect(page.getByLabel("Font size")).toBeVisible();
    await expect(page.getByRole("combobox", { name: "Local shell", exact: true })).toHaveCount(0);
  });
});

test.describe("Keep sessions alive in the background", () => {
  const LABEL = "Keep sessions alive in the background";
  const toggle = (page: Page) => page.getByRole("checkbox", { name: LABEL });
  const keepAlive = (page: Page) => page.evaluate(() => window.__tetherMock.keepAliveStatus());

  test("off_by_default_on_after_permission_runs_only_while_a_session_is_open", async ({ page }) => {
    await withHost(page);
    await goTo(page, "Settings");
    await expect(toggle(page)).toBeEnabled();
    await expect(toggle(page)).not.toBeChecked();
    await expect(
      page.getByText(
        "Uses location in the background so iOS keeps Termlyx running while a session or port forward is open. Uses more battery. Termlyx never stores or sends your location.",
      ),
    ).toBeVisible();

    await toggle(page).click();
    await expect(toggle(page)).toBeChecked();
    expect(await page.evaluate(() => window.__tetherMock.control.locationAsks())).toBe(1);
    expect(await keepAlive(page)).toEqual({ enabled: true, authorized: true, running: false });

    // A session open: running; the session gone: stopped at once.
    await goTo(page, "Hosts");
    await openWeb1(page);
    expect((await keepAlive(page)).running).toBe(true);
    const [id] = await sessionIds(page);
    await page.evaluate((sid) => window.__tetherMock.control.endSession(sid, "connection lost"), id);
    await expect.poll(async () => (await keepAlive(page)).running).toBe(false);

    // Kept for the device: a fresh start with the permission still given turns it back on,
    // without asking.
    await page.addInitScript(() => {
      window.__tetherMockSeed = { locationPermission: "granted" };
    });
    await page.reload();
    await expect.poll(async () => (await keepAlive(page)).enabled).toBe(true);
    expect(await page.evaluate(() => window.__tetherMock.control.locationAsks())).toBe(0);
  });

  test("start_up_never_asks_and_forgets_the_choice_without_permission", async ({ page }) => {
    await createVault(page);
    await goTo(page, "Settings");
    await toggle(page).click();
    await expect(toggle(page)).toBeChecked();
    // A fresh start where the permission is no longer given ("Allow Once" ended).
    await page.reload();
    await createVault(page);
    expect(await page.evaluate(() => window.__tetherMock.control.locationAsks())).toBe(0);
    expect(await page.evaluate(() => localStorage.getItem("tether.keepalive"))).toBeNull();
    await goTo(page, "Settings");
    await expect(toggle(page)).toBeEnabled();
    await expect(toggle(page)).not.toBeChecked();
  });

  test("permission_lapsing_while_settings_is_open_unchecks_it_with_the_note", async ({ page }) => {
    await createVault(page);
    await goTo(page, "Settings");
    await toggle(page).click();
    await expect(toggle(page)).toBeChecked();
    await page.evaluate(() => window.__tetherMock.control.setLocationPermission("notDetermined"));
    await expect(toggle(page)).not.toBeChecked();
    await expect(
      page.getByRole("alert").filter({
        hasText: "Location permission was not given, so Termlyx can't stay running in the background.",
      }),
    ).toBeVisible();
    expect(await page.evaluate(() => localStorage.getItem("tether.keepalive"))).toBeNull();
  });

  test("denied_permission_turns_it_back_off_with_a_note", async ({ page }) => {
    await page.addInitScript(() => {
      window.__tetherMockSeed = { locationAnswer: "deny" };
    });
    await createVault(page);
    await goTo(page, "Settings");
    await expect(toggle(page)).toBeEnabled();
    await toggle(page).click();
    await expect(
      page.getByRole("alert").filter({
        hasText: "Location permission was not given, so Termlyx can't stay running in the background.",
      }),
    ).toBeVisible();
    await expect(toggle(page)).not.toBeChecked();
    expect((await keepAlive(page)).enabled).toBe(false);

    // Nothing kept: a fresh start does not ask again.
    await page.reload();
    await expect(page.getByRole("heading", { name: "Create your vault" })).toBeVisible();
    expect(await page.evaluate(() => window.__tetherMock.control.locationAsks())).toBe(0);
  });
});

test("a_forwarding_rule_row_keeps_its_text_readable", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "Servarr", "192.168.1.13");
  await goTo(page, "Port Forwarding");
  await page.getByRole("button", { name: "New rule" }).click();
  const dialog = page.getByRole("dialog", { name: "New rule" });
  await dialog.getByLabel("Host", { exact: true }).selectOption({ label: "Servarr" });
  await dialog.getByLabel("Label").fill("Test");
  await dialog.getByLabel("Bind port").fill("8989");
  await dialog.getByLabel("Destination host").fill("192.168.1.13");
  await dialog.getByLabel("Destination port").fill("8989");
  await dialog.getByLabel("Start automatically").check();
  await dialog.getByRole("button", { name: "Save rule" }).click();
  await expect(dialog).toBeHidden();
  const row = page.getByRole("list", { name: "Forwarding rules" }).getByRole("listitem");
  await row.getByRole("button", { name: "Start" }).click();
  await expect(row.getByRole("status")).toHaveText("Active · 127.0.0.1:8989");

  // The label, summary and status each fit on one line, beside nothing that squeezes them.
  const rowBox = (await row.boundingBox())!;
  const summary = row.getByText("127.0.0.1:8989 → 192.168.1.13:8989");
  for (const text of [row.getByText("Test", { exact: true }), summary, row.getByRole("status")]) {
    const box = (await text.boundingBox())!;
    expect(box.height).toBeLessThan(30);
  }
  expect((await summary.boundingBox())!.width).toBeGreaterThan(rowBox.width / 2);
  // Nothing overlaps: the status sits below the text, the buttons below or beside the status.
  const main = (await row.locator(".item-main").boundingBox())!;
  const status = (await row.getByRole("status").boundingBox())!;
  expect(status.y).toBeGreaterThanOrEqual(main.y + main.height - 1);
});

test("a_dialog_field_focused_with_the_keyboard_up_stays_above_the_buttons", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "192.168.1.6");
  await goTo(page, "Port Forwarding");
  await page.getByRole("button", { name: "New rule" }).click();
  const dialog = page.getByRole("dialog", { name: "New rule" });
  const actions = dialog.locator(".dialog-actions");
  // As on iOS: the field takes focus first, then the keyboard opens and the sheet shrinks.
  // 59 more: the top safe area (Dynamic Island) the sheet also gives up on a device.
  for (const name of ["Bind port", "Destination host", "Destination port"]) {
    await softKeyboard(page, 0);
    await dialog.getByLabel(name).focus();
    await softKeyboard(page, IPHONE_KEYBOARD_PX + 59);
    await expect
      .poll(async () => {
        const field = (await dialog.getByLabel(name).boundingBox())!;
        const bar = (await actions.boundingBox())!;
        return field.y + field.height <= bar.y;
      }, { message: name })
      .toBe(true);
  }
  await dialog.getByLabel("Destination host").fill("localhost");
  await expect(dialog.getByLabel("Destination host")).toHaveValue("localhost");
});
