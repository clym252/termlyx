import { expect, test } from "./fixtures";
import type { Page } from "@playwright/test";
import {
  IPAD_AIR_LANDSCAPE,
  IPAD_AIR_PORTRAIT,
  IPAD_KEYBOARD_PX,
  KEYS,
  activePane,
  createVault,
  emulateIos,
  goTo,
  hostRow,
  nav,
  panes,
  seedHost,
  sessionIds,
  softKeyboard,
  terminalPanel,
  terminalTabs,
} from "./helpers";

const sidebar = (page: Page) => page.locator("aside.sidebar");
const keyBar = (page: Page) => page.getByRole("toolbar", { name: "Terminal keys" });

/** A vault with hosts web-1 and web-2 on an iPad of `size`. */
async function ipadWithHosts(page: Page, size = IPAD_AIR_LANDSCAPE) {
  await emulateIos(page, size);
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await seedHost(page, "web-2", "web2.example");
}

for (const [name, size] of [
  ["landscape", IPAD_AIR_LANDSCAPE],
  ["portrait", IPAD_AIR_PORTRAIT],
] as const) {
  test.describe(`iPad Air ${name}`, () => {
    test("the_sidebar_is_the_icon_rail_by_default_and_expands", async ({ page }) => {
      await emulateIos(page, size);
      await createVault(page);
      await expect(page.locator(".layout-phone")).toHaveCount(0);
      await expect(sidebar(page)).toHaveAttribute("data-mode", "rail");
      expect(Math.round((await sidebar(page).boundingBox())!.width)).toBe(48);
      await expect(nav(page).getByRole("button", { name: "Keychain", exact: true })).toBeVisible();

      await page.getByRole("button", { name: "Expand sidebar" }).click();
      await expect(sidebar(page)).toHaveAttribute("data-mode", "full");
      await expect(nav(page).getByText("Keychain")).toHaveCount(1);
      // The chord toggles it back.
      await page.keyboard.press("Meta+Backslash");
      await expect(sidebar(page)).toHaveAttribute("data-mode", "rail");
    });

    test("two_hosts_open_as_two_tabs_on_top", async ({ page }) => {
      await ipadWithHosts(page, size);
      await hostRow(page, "web-1").click();
      await expect(activePane(page)).toHaveAttribute("data-status", "open");
      await nav(page).getByRole("button", { name: "Hosts", exact: true }).click();
      await hostRow(page, "web-2").click();
      await expect(terminalTabs(page)).toHaveText(["web-1", "web-2"]);
      await expect(terminalTabs(page).nth(1)).toHaveAttribute("aria-selected", "true");

      // The strip sits above the terminal.
      const strip = (await page.getByRole("tablist", { name: "Terminal tabs" }).boundingBox())!;
      const panel = (await terminalPanel(page).boundingBox())!;
      expect(strip.y + strip.height).toBeLessThanOrEqual(panel.y + 1);
      expect(await sessionIds(page)).toHaveLength(2);
    });
  });
}

test("ipad_split_shortcut_opens_second_pane", async ({ page }) => {
  await ipadWithHosts(page, IPAD_AIR_LANDSCAPE);
  await hostRow(page, "web-1").click();
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await activePane(page).locator(".xterm").click();
  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(2);
  await expect(page.locator(".split-handle")).toHaveCount(1);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  // Pane focus works too.
  const ids = await panes(page).evaluateAll((els) => els.map((e) => e.getAttribute("data-pane-id")!));
  await expect(activePane(page)).toHaveAttribute("data-pane-id", ids[1]);
  await page.keyboard.press(KEYS.focus("ArrowLeft"));
  await expect(activePane(page)).toHaveAttribute("data-pane-id", ids[0]);
});

test.describe("touch", () => {
  test.use({ hasTouch: true });
  test("ipad_layout_menu_splits_by_touch", async ({ page }) => {
    await ipadWithHosts(page, IPAD_AIR_LANDSCAPE);
    await hostRow(page, "web-1").click();
    await expect(activePane(page)).toHaveAttribute("data-status", "open");
    await page.getByRole("button", { name: "Layout", exact: true }).tap();
    await page.getByRole("menuitem", { name: /^Split right(?!…)/ }).tap();
    await expect(panes(page)).toHaveCount(2);
    await expect.poll(() => sessionIds(page)).toHaveLength(2);
  });
});

test("a_tab_can_be_dragged_to_reorder", async ({ page }) => {
  await ipadWithHosts(page);
  await hostRow(page, "web-1").click();
  await nav(page).getByRole("button", { name: "Hosts", exact: true }).click();
  await hostRow(page, "web-2").click();
  await expect(terminalTabs(page)).toHaveText(["web-1", "web-2"]);
  const first = (await terminalTabs(page).nth(0).locator("xpath=..").boundingBox())!;
  await terminalTabs(page).nth(1).dragTo(terminalTabs(page).nth(0), { targetPosition: { x: 2, y: first.height / 2 } });
  await expect(terminalTabs(page)).toHaveText(["web-2", "web-1"]);
});

test("the_new_tab_chord_works_and_offers_hosts_only", async ({ page }) => {
  await ipadWithHosts(page);
  await hostRow(page, "web-1").click();
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await page.keyboard.press(KEYS.newTab);
  await expect(terminalTabs(page)).toHaveCount(2);
  await expect(page.getByRole("heading", { name: "New tab" })).toBeVisible();
  await expect(page.getByRole("list", { name: "Connect to a host" })).toBeVisible();
  await expect(page.getByText("Local terminal")).toHaveCount(0);
  // Close the empty tab with the chord.
  await page.keyboard.press(KEYS.close);
  await expect(terminalTabs(page)).toHaveCount(1);
});

test("the_local_terminal_chord_does_nothing", async ({ page }) => {
  await ipadWithHosts(page);
  await hostRow(page, "web-1").click();
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await activePane(page).locator(".xterm").click();
  await page.keyboard.press(KEYS.localTerminal);
  await expect(terminalTabs(page)).toHaveCount(1);
  expect(await sessionIds(page)).toHaveLength(1);
});

test("the_key_bar_shows_only_with_the_software_keyboard", async ({ page }) => {
  await ipadWithHosts(page);
  await hostRow(page, "web-1").click();
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await activePane(page).locator(".xterm").click();
  await expect(activePane(page).locator("textarea.xterm-helper-textarea")).toBeFocused();
  // A hardware keyboard: the visual viewport stays full height.
  await expect(keyBar(page)).toHaveCount(0);
  await softKeyboard(page, IPAD_KEYBOARD_PX);
  await expect(keyBar(page)).toBeVisible();
  await softKeyboard(page, 0);
  await expect(keyBar(page)).toHaveCount(0);
});

test("a_slide_over_width_gets_the_phone_layout", async ({ page }) => {
  await emulateIos(page, IPAD_AIR_PORTRAIT);
  await page.setViewportSize({ width: 507, height: 1180 });
  await createVault(page);
  await expect(page.locator(".layout-phone")).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Menu", exact: true })).toBeVisible();
  await expect(sidebar(page)).toHaveCount(0);
  // Widening to the full screen brings the rail back.
  await page.setViewportSize(IPAD_AIR_PORTRAIT);
  await expect(page.locator(".layout-phone")).toHaveCount(0);
  await expect(sidebar(page)).toHaveAttribute("data-mode", "rail");
});

test("split_tab_survives_slide_over_and_back", async ({ page }) => {
  await ipadWithHosts(page, IPAD_AIR_LANDSCAPE);
  await hostRow(page, "web-1").click();
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await activePane(page).locator(".xterm").click();
  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  const ids = await sessionIds(page);
  const paneIds = await panes(page).evaluateAll((els) => els.map((e) => e.getAttribute("data-pane-id")!));

  // Slide Over: the phone layout shows the focused pane alone, with no pane header.
  await page.setViewportSize({ width: 500, height: 820 });
  await expect(page.locator(".layout-phone")).toHaveCount(1);
  await expect(panes(page).and(page.locator(":visible"))).toHaveCount(1);
  await expect(page.locator(".pane-header:visible")).toHaveCount(0);

  // The drawer's Terminals lists the tab's two panes; the second one's row shows that pane.
  await page.getByRole("button", { name: "Back to hosts", exact: true }).click();
  await page.getByRole("button", { name: "Menu", exact: true }).click();
  await page.getByRole("dialog", { name: "Menu" }).getByRole("button", { name: "Terminals (1)" }).click();
  const rows = page.getByRole("list", { name: "Open sessions" }).locator("li.phone-pane");
  await expect(rows).toHaveCount(2);
  await rows.nth(1).locator(".phone-pane-main").click();
  await expect(panes(page).and(page.locator(":visible"))).toHaveCount(1);
  await expect(panes(page).and(page.locator(":visible"))).toHaveAttribute("data-pane-id", paneIds[1]);
  // The bar names the focused pane.
  await expect(page.locator(".phone-top-title")).toHaveText("web-1 (2)");

  // Widening back: both panes, the same sessions.
  await page.setViewportSize(IPAD_AIR_LANDSCAPE);
  await expect(page.locator(".layout-phone")).toHaveCount(0);
  await expect(panes(page).and(page.locator(":visible"))).toHaveCount(2);
  expect(await sessionIds(page)).toEqual(ids);
});

/** A split tab (web-1 twice) on an iPad that then narrows to Slide Over: the phone layout. Returns the session ids. */
async function splitTabInSlideOver(page: Page) {
  await ipadWithHosts(page, IPAD_AIR_LANDSCAPE);
  await hostRow(page, "web-1").click();
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await activePane(page).locator(".xterm").click();
  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  const ids = await sessionIds(page);
  await page.setViewportSize({ width: 500, height: 820 });
  await expect(page.locator(".layout-phone")).toHaveCount(1);
  return ids;
}

test("the_phone_top_bar_closes_only_the_shown_pane_of_a_split_tab", async ({ page }) => {
  const [first, second] = await splitTabInSlideOver(page);
  await expect(page.locator(".phone-top-title")).toHaveText("web-1 (2)");

  await page.getByRole("button", { name: "Session menu" }).click();
  await page.getByRole("menu", { name: "Session menu" }).getByRole("menuitem", { name: "Close session" }).click();
  // The other pane is left, shown, and its session goes on.
  await expect(panes(page)).toHaveCount(1);
  await expect(page.locator(".phone-top-title")).toHaveText("web-1");
  await expect.poll(() => sessionIds(page)).toEqual([first]);
  expect(second).not.toBe(first);
  await expect(activePane(page)).toHaveAttribute("data-status", "open");

  // Now a one-pane tab: Close session ends it, back to the hosts.
  await page.getByRole("button", { name: "Session menu" }).click();
  await page.getByRole("menu", { name: "Session menu" }).getByRole("menuitem", { name: "Close session" }).click();
  await expect.poll(() => sessionIds(page)).toHaveLength(0);
  await expect(page.getByRole("button", { name: "Menu", exact: true })).toBeVisible();
});

test("the_drawer_closes_a_split_tab_or_one_of_its_panes", async ({ page }) => {
  const [first] = await splitTabInSlideOver(page);
  const openSessions = async () => {
    await page.getByRole("button", { name: "Back to hosts", exact: true }).click();
    await page.getByRole("button", { name: "Menu", exact: true }).click();
    await page.getByRole("dialog", { name: "Menu" }).getByRole("button", { name: /^Terminals/ }).click();
    return page.getByRole("list", { name: "Open sessions" });
  };
  const list = await openSessions();

  // The tab's row says what its Close ends; each pane has its own Close.
  const tabClose = list.getByRole("button", { name: "Close tab (2 sessions)" });
  await expect(tabClose).toHaveCount(1);
  await expect(list.getByRole("button", { name: "Close session web-1", exact: true })).toHaveCount(1);
  const paneClose = list.getByRole("button", { name: "Close session web-1 (2)", exact: true });
  await expect(paneClose).toHaveCount(1);

  // A pane's Close ends that session only.
  await paneClose.focus();
  await expect(list.locator("li.phone-pane").nth(1)).toHaveAttribute("data-revealed", "true");
  await paneClose.click();
  await expect.poll(() => sessionIds(page)).toEqual([first]);
  await expect(list.locator("li.phone-pane")).toHaveCount(0);
  await expect(list.getByRole("button", { name: "Close session web-1", exact: true })).toHaveCount(1);

  // Split again: the tab's Close ends every pane.
  await page.setViewportSize(IPAD_AIR_LANDSCAPE);
  await page.getByRole("button", { name: "Terminals", exact: false }).first().click();
  await expect(page.locator(".layout-phone")).toHaveCount(0);
  await page.getByRole("tab", { name: "web-1" }).click();
  await page.locator(".terminal-pane[data-active=\"true\"] .xterm").click();
  await page.keyboard.press(KEYS.splitRight);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  await page.setViewportSize({ width: 500, height: 820 });
  const again = await openSessions();
  const close = again.getByRole("button", { name: "Close tab (2 sessions)" });
  await close.focus();
  await close.click();
  await expect.poll(() => sessionIds(page)).toHaveLength(0);
});

test("picking_a_host_for_a_new_pane_has_no_details_button", async ({ page }) => {
  await ipadWithHosts(page, IPAD_AIR_LANDSCAPE);
  await hostRow(page, "web-1").click();
  await expect(activePane(page)).toHaveAttribute("data-status", "open");

  // The palette lists hosts with a Details button (no Alt+Enter on a touch screen).
  await page.keyboard.press("Meta+k");
  const search = page.getByRole("dialog", { name: "Search" });
  await expect(search.getByRole("button", { name: "Details" })).toHaveCount(2);
  await page.keyboard.press("Escape");
  await expect(search).toHaveCount(0);

  // Picking the host for a new pane: a tap splits, so no Details beside it.
  await page.getByRole("button", { name: "Layout", exact: true }).click();
  await page.getByRole("menuitem", { name: "Split right…" }).click();
  const pick = page.getByRole("dialog", { name: "Open in new pane" });
  await expect(pick.getByRole("option")).toHaveCount(2);
  await expect(pick.getByRole("button", { name: "Details" })).toHaveCount(0);
  await pick.getByRole("option", { name: /web-2/ }).click();
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
});

test("a_long_press_on_a_rail_icon_shows_its_name_and_does_not_navigate", async ({ page }) => {
  await emulateIos(page, IPAD_AIR_LANDSCAPE);
  await createVault(page);
  const keychain = nav(page).getByRole("button", { name: "Keychain", exact: true });
  const press = (type: string) => keychain.dispatchEvent(type, { pointerType: "touch", pointerId: 7, isPrimary: true, bubbles: true });
  const tip = page.locator(".rail-tip");

  // A tap is not a long press.
  await press("pointerdown");
  await press("pointerup");
  await expect(tip).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();

  // Held: the name appears beside the icon.
  await press("pointerdown");
  await expect(tip).toHaveText("Keychain");
  const icon = (await keychain.boundingBox())!;
  const bubble = (await tip.boundingBox())!;
  expect(bubble.x).toBeGreaterThanOrEqual(icon.x + icon.width - 1);
  // Lifting the finger hides it, and the click that follows a long press does not open the page.
  await press("pointerup");
  await keychain.dispatchEvent("click");
  await expect(tip).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  // The next plain tap still navigates.
  await keychain.click();
  await expect(page.getByRole("heading", { name: "Keychain" })).toBeVisible();
});

test("the_drawer_traps_the_keyboard_focus", async ({ page }) => {
  await emulateIos(page, IPAD_AIR_PORTRAIT);
  await page.setViewportSize({ width: 507, height: 1180 });
  await createVault(page);
  await page.getByRole("button", { name: "Menu", exact: true }).click();
  const drawer = page.getByRole("dialog", { name: "Menu" });
  await expect(drawer).toBeVisible();
  await expect(page.locator("main.main")).toHaveAttribute("inert", "");
  // Tab around more times than the drawer has controls: focus never lands in the page behind.
  for (let i = 0; i < 25; i++) {
    await page.keyboard.press("Tab");
    const inPageBehind = await page.evaluate(() => !!document.activeElement?.closest("main.main"));
    expect(inPageBehind).toBe(false);
  }
  await page.keyboard.press("Escape");
  await expect(drawer).toHaveCount(0);
  await expect(page.locator("main.main")).not.toHaveAttribute("inert", "");
  // The focus goes back to what opened it.
  await expect(page.getByRole("button", { name: "Menu", exact: true })).toBeFocused();
});

test("the_sftp_local_pane_is_named_for_the_ipad", async ({ page }) => {
  await ipadWithHosts(page);
  await goTo(page, "SFTP");
  await page.getByLabel("Host", { exact: true }).selectOption({ label: "web-1" });
  await expect(page.getByRole("region", { name: "Local files" }).getByRole("heading", { name: "On My iPad › Termlyx" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Local files" }).getByRole("button", { name: "Import from Files", exact: true })).toBeVisible();
});
