import { expect, test } from "./fixtures";
import { IPAD_AIR_LANDSCAPE, createVault, emulateIos, hostRow, nav, seedHost, terminalTabs, workspaceKeys } from "./helpers";

const sidebar = (page: import("@playwright/test").Page) => page.locator("aside.sidebar");
const width = async (page: import("@playwright/test").Page) =>
  Math.round((await sidebar(page).boundingBox())!.width);

test("sidebar_collapses_to_a_rail_and_stays_that_way_after_a_reload", async ({ page }) => {
  await createVault(page);
  await expect(sidebar(page)).toHaveAttribute("data-mode", "full");
  await expect.poll(() => width(page)).toBe(200);

  await page.getByRole("button", { name: "Collapse sidebar" }).click();
  await expect(sidebar(page)).toHaveAttribute("data-mode", "rail");
  await expect.poll(() => width(page)).toBe(48);
  // No words in the rail; the names are still there.
  await expect(nav(page).getByText("Keychain")).toHaveCount(0);
  await expect(nav(page).getByRole("button", { name: "Keychain", exact: true })).toHaveAttribute("title", "Keychain");

  // The device remembers it.
  await page.reload();
  await expect(page.getByRole("heading", { name: "Create your vault" })).toBeVisible();
  await createVault(page);
  await expect(sidebar(page)).toHaveAttribute("data-mode", "rail");
  await expect.poll(() => width(page)).toBe(48);

  await page.getByRole("button", { name: "Expand sidebar" }).click();
  await expect(sidebar(page)).toHaveAttribute("data-mode", "full");
  await expect(nav(page).getByText("Keychain")).toHaveCount(1);
});

test("rail_icons_navigate_lock_and_count_terminals", async ({ page }) => {
  await createVault(page);
  await page.getByRole("button", { name: "Collapse sidebar" }).click();

  await nav(page).getByRole("button", { name: "Keychain", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Keychain" })).toBeVisible();
  await expect(nav(page).getByRole("button", { name: "Keychain", exact: true })).toHaveAttribute("aria-current", "page");

  // A terminal shows as a count on its icon.
  await page.keyboard.press(workspaceKeys(true).localTerminal);
  const terminals = nav(page).getByRole("button", { name: "Terminals (1)" });
  await expect(terminals).toHaveText("1");
  await expect(terminals).toHaveAttribute("aria-current", "page");

  await page.getByRole("button", { name: "Lock vault" }).click();
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
});

test("sidebar_toggle_chord_on_macos", async ({ page }) => {
  await createVault(page);
  await expect(page.getByRole("button", { name: "Collapse sidebar" })).toHaveAttribute("title", "Collapse sidebar (⌘\\)");
  await page.keyboard.press("Meta+Backslash");
  await expect(sidebar(page)).toHaveAttribute("data-mode", "rail");
  await page.keyboard.press("Meta+Backslash");
  await expect(sidebar(page)).toHaveCount(0);
  await page.keyboard.press("Meta+Backslash");
  await expect(sidebar(page)).toHaveAttribute("data-mode", "full");
});

test.describe("on Windows", () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      Object.defineProperty(Navigator.prototype, "platform", { get: () => "Win32", configurable: true });
    });
  });

  test("windows_toggle_names_ctrl_shift_b_and_plain_ctrl_b_does_nothing", async ({ page }) => {
    await createVault(page);
    await expect(page.getByRole("button", { name: "Collapse sidebar" })).toHaveAttribute(
      "title",
      "Collapse sidebar (Ctrl+Shift+B)",
    );
    await page.keyboard.press("Control+b");
    await expect(sidebar(page)).toHaveAttribute("data-mode", "full");
    await page.keyboard.press("Control+Shift+b");
    await expect(sidebar(page)).toHaveAttribute("data-mode", "rail");
    await expect(page.getByRole("button", { name: "Expand sidebar" })).toHaveAttribute(
      "title",
      "Expand sidebar (Ctrl+Shift+B)",
    );
    const text = await page.locator("body").innerText();
    expect(text).not.toContain("⌘");
  });
});

type Page = import("@playwright/test").Page;
const showButton = (page: Page) => page.getByRole("button", { name: "Show sidebar" });
const menuButton = (page: Page) => page.getByRole("button", { name: "Menu" });
const drawer = (page: Page) => page.getByRole("dialog", { name: "Menu" });

/** `target` does not sit under the ☰ button. */
async function expectClear(page: Page, target: import("@playwright/test").Locator) {
  const b = (await showButton(page).boundingBox())!;
  const t = (await target.boundingBox())!;
  expect(t.x).toBeGreaterThanOrEqual(b.x + b.width);
}

/** `opener` (☰) opens the full sidebar in the drawer, without the full/rail toggle. */
async function expectDrawerOpens(page: Page, opener: import("@playwright/test").Locator) {
  await opener.click();
  await expect(drawer(page)).toBeVisible();
  await expect(drawer(page).locator("aside.sidebar")).toHaveAttribute("data-mode", "full");
  await expect(drawer(page).getByRole("button", { name: "Collapse sidebar" })).toHaveCount(0);
}

/** A hidden sidebar's page: the phone's top bar above the page, which scrolls under it. */
async function expectPageBar(page: Page, title: string) {
  await expect(menuButton(page)).toBeVisible();
  await expect(page.locator(".phone-top-title")).toHaveText(title);
  await expect(page.getByRole("heading", { name: title, level: 1 })).toHaveCount(1);
  const bar = (await page.locator(".phone-top").boundingBox())!;
  const body = (await page.locator(".page:visible").first().boundingBox())!;
  expect(body.y).toBeGreaterThanOrEqual(bar.y + bar.height);
}

async function hideFromSettings(page: Page) {
  await nav(page).getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("radiogroup", { name: "Sidebar" }).getByRole("radio", { name: "Hidden" }).click();
  await expect(sidebar(page)).toHaveCount(0);
}

test("hidden_sidebar_opens_as_a_drawer_and_the_chord_cycles_all_three", async ({ page }) => {
  await createVault(page);
  await page.keyboard.press("Meta+Backslash");
  await expect(sidebar(page)).toHaveAttribute("data-mode", "rail");
  await page.keyboard.press("Meta+Backslash");
  await expect(sidebar(page)).toHaveCount(0);
  await expectPageBar(page, "Hosts");
  await page.keyboard.press("Meta+Backslash");
  await expect(sidebar(page)).toHaveAttribute("data-mode", "full");

  // Settings -> Hidden: ☰ opens the whole sidebar in a drawer over the page; picking closes it, still hidden.
  await page.getByRole("button", { name: "Collapse sidebar" }).click();
  await hideFromSettings(page);
  await expectPageBar(page, "Settings");
  await page.locator(".settings-page").evaluate((e) => (e.scrollTop = e.scrollHeight));
  await expectPageBar(page, "Settings");
  await expectDrawerOpens(page, menuButton(page));
  await nav(page).getByRole("button", { name: "Hosts", exact: true }).click();
  await expect(drawer(page)).toHaveCount(0);
  await expect(sidebar(page)).toHaveCount(0);
  await expectPageBar(page, "Hosts");
  await page.screenshot({ path: "test-results/sidebar-hidden-hosts.png" });
  await expectDrawerOpens(page, menuButton(page));
  await page.keyboard.press("Escape");
  await expect(drawer(page)).toHaveCount(0);

  // Terminals: no page bar; ☰ leads the tab bar.
  await page.keyboard.press(workspaceKeys(true).localTerminal);
  await expect(page.locator(".phone-top")).toHaveCount(0);
  await expect(showButton(page)).toHaveAttribute("title", "Show sidebar (⌘\\)");
  await expectClear(page, terminalTabs(page).first());
  await expectDrawerOpens(page, showButton(page));
  await page.keyboard.press("Escape");
  await expect(drawer(page)).toHaveCount(0);

  // The chord leaves hidden for full, then back through rail; neither has a ☰.
  await page.keyboard.press("Meta+Backslash");
  await expect(sidebar(page)).toHaveAttribute("data-mode", "full");
  await expect(showButton(page)).toHaveCount(0);
  await page.keyboard.press("Meta+Backslash");
  await expect(sidebar(page)).toHaveAttribute("data-mode", "rail");
});

test.describe("on an iPad", () => {
  test("the_menu_button_is_on_settings_and_on_terminals", async ({ page }) => {
    await emulateIos(page, IPAD_AIR_LANDSCAPE);
    await createVault(page);
    await seedHost(page, "web-1", "web1.example");
    await hideFromSettings(page);
    await expectPageBar(page, "Settings");
    // ☰ opens the sidebar in a drawer, like a phone; the terminals page has ☰ first in the tab bar.
    await expectDrawerOpens(page, menuButton(page));
    await nav(page).getByRole("button", { name: "Hosts", exact: true }).click();
    await expect(drawer(page)).toHaveCount(0);
    // A narrow card's Edit and Connect cover its middle: tap the name.
    await hostRow(page, "web-1").click({ position: { x: 20, y: 25 } });
    await expect(terminalTabs(page)).toHaveText(["web-1"]);
    await expect(sidebar(page)).toHaveCount(0);
    await expectClear(page, terminalTabs(page).first());
    await page.screenshot({ path: "test-results/sidebar-hidden-ipad-terminals.png" });
    await expectDrawerOpens(page, showButton(page));
    await page.getByTestId("drawer-backdrop").click({ position: { x: 900, y: 300 } });
    await expect(drawer(page)).toHaveCount(0);
  });
});
