import { expect, test } from "./fixtures";
import {
  activePane,
  connectHost,
  createVault,
  hostForm,
  panes,
  seedHost,
  KEYS,
  sessionIds,
  terminalTabs,
  written,
} from "./helpers";
import type { Page } from "@playwright/test";

const palette = (page: Page) => page.getByRole("dialog", { name: "Search" });
const box = (page: Page) => palette(page).getByRole("combobox", { name: "Search" });

async function search(page: Page, query: string) {
  await page.keyboard.press("Meta+k");
  await expect(palette(page)).toBeVisible();
  await box(page).fill(query);
}

test("enter_connects_to_host", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await search(page, "web1.exa");
  await expect(palette(page).getByRole("group", { name: "Hosts" }).getByRole("option")).toHaveCount(1);
  await box(page).press("Enter");
  await expect(palette(page)).toBeHidden();
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect(terminalTabs(page).first()).toContainText("web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  // The terminal has the keyboard back.
  await expect(activePane(page).locator("textarea")).toBeFocused();
});

test("mod_enter_splits_right_with_host", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await seedHost(page, "web-2", "web2.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");

  await search(page, "web-2");
  await box(page).press("Meta+Enter");
  await expect(palette(page)).toBeHidden();
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  // The new pane (web-2) is the focused one; the tab is named after it.
  await expect(terminalTabs(page).first()).toContainText("web-2");
});

test("mod_enter_without_a_split_target_opens_a_tab", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await search(page, "web-1");
  await box(page).press("Meta+Enter");
  await expect(palette(page)).toBeHidden();
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect(panes(page)).toHaveCount(1);
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
});

test("alt_enter_opens_host_details", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await search(page, "web-1");
  await box(page).press("Alt+Enter");
  await expect(palette(page)).toBeHidden();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  await expect(hostForm(page).getByLabel("Label")).toHaveValue("web-1");
  await expect(terminalTabs(page)).toHaveCount(0);
});

test("switch_to_open_pane", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await seedHost(page, "web-2", "web2.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await connectHost(page, "web-2");
  await expect(terminalTabs(page)).toHaveCount(2);
  await expect(terminalTabs(page).nth(1)).toHaveAttribute("aria-selected", "true");

  // The open terminal comes before the host of the same name.
  await search(page, "web-1");
  const terminals = palette(page).getByRole("group", { name: "Open terminals" });
  await expect(terminals.getByRole("option")).toHaveCount(1);
  await box(page).press("Enter");
  await expect(palette(page)).toBeHidden();
  await expect(terminalTabs(page).first()).toHaveAttribute("aria-selected", "true");
  await expect(terminalTabs(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
});

test("action_runs_layout_preset", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");

  await search(page, "2 columns");
  const actions = palette(page).getByRole("group", { name: "Actions" });
  await expect(actions.getByRole("option")).toHaveText([/Layout: 2 columns/]);
  await box(page).press("Enter");
  await expect(palette(page)).toBeHidden();
  await expect(panes(page)).toHaveCount(2);
  await expect(panes(page).and(page.locator('[data-launcher="true"]'))).toHaveCount(1);
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
});

test("split_right_pick_opens_chosen_host", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await seedHost(page, "web-2", "web2.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");

  await page.getByRole("button", { name: "Layout", exact: true }).click();
  await page.getByRole("menuitem", { name: "Split right…" }).click();
  const pick = page.getByRole("dialog", { name: "Open in new pane" });
  await expect(pick).toBeVisible();
  // Only hosts and the local terminal; the input has the keyboard (the menu does not take it back).
  await expect(pick.getByRole("group")).toHaveCount(2);
  await expect(pick.getByRole("combobox", { name: "Search" })).toBeFocused();
  await pick.getByRole("combobox", { name: "Search" }).fill("web-2");
  await pick.getByRole("combobox", { name: "Search" }).press("Enter");
  await expect(pick).toBeHidden();
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  await expect(terminalTabs(page).first()).toContainText("web-2");

  // Escape cancels: no new pane.
  await page.getByRole("button", { name: "Layout", exact: true }).click();
  await page.getByRole("menuitem", { name: "Split down…" }).click();
  await expect(pick).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(pick).toBeHidden();
  await expect(panes(page)).toHaveCount(2);
});

test("mod_enter_on_an_open_terminal_splits_onto_its_host", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");

  // The open terminal sorts before the host of the same name: the footer's split applies to it.
  await search(page, "web-1");
  const terminals = palette(page).getByRole("group", { name: "Open terminals" });
  await expect(terminals.getByRole("option")).toHaveCount(1);
  await expect(box(page)).toHaveAttribute("aria-activedescendant", /tab:/);
  await box(page).press("Meta+Enter");
  await expect(palette(page)).toBeHidden();
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
});

test("mod_enter_on_a_terminal_without_a_split_target_opens_a_tab", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  // The shown tab is an empty launcher: nothing to split.
  await page.keyboard.press(KEYS.newTab);
  await expect(terminalTabs(page)).toHaveCount(2);

  await search(page, "web-1");
  await box(page).press("Meta+Enter");
  await expect(palette(page)).toBeHidden();
  await expect(terminalTabs(page)).toHaveCount(3);
  await expect(terminalTabs(page).nth(2)).toContainText("web-1");
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
});

test("alt_enter_on_an_open_terminal_shows_its_host_details", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");

  await search(page, "web-1");
  await box(page).press("Alt+Enter");
  await expect(palette(page)).toBeHidden();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  await expect(hostForm(page).getByLabel("Label")).toHaveValue("web-1");
  // Nothing opened or closed (the terminals are behind the page).
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: /^Terminals \(1\)/ })).toBeVisible();
});

test("alt_enter_on_a_local_terminal_does_nothing", async ({ page }) => {
  await createVault(page);
  await page.keyboard.press(KEYS.localTerminal);
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect(activePane(page)).toHaveAttribute("data-status", "open");

  await search(page, "Local shell");
  await expect(palette(page).getByRole("group", { name: "Open terminals" }).getByRole("option")).toHaveCount(1);
  await box(page).press("Alt+Enter");
  await expect(palette(page)).toBeVisible();
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect(panes(page)).toHaveCount(1);
});

test("escape_gives_the_keyboard_back_to_the_active_terminal", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  const [id] = await sessionIds(page);
  const typed = async (text: string) => {
    await page.keyboard.type(text);
    await expect.poll(() => written(page, id)).toContain(text);
  };

  // From the Layout menu's Split right…: the cancel leaves the terminal focused.
  await page.getByRole("button", { name: "Layout", exact: true }).click();
  await page.getByRole("menuitem", { name: "Split right…" }).click();
  await expect(page.getByRole("dialog", { name: "Open in new pane" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(activePane(page).locator("textarea")).toBeFocused();
  await typed("ab");

  // From the chord inside the terminal.
  await page.keyboard.press("Meta+k");
  await expect(palette(page)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(palette(page)).toBeHidden();
  await expect(activePane(page).locator("textarea")).toBeFocused();
  await typed("cd");
  expect(await written(page, id)).toBe("abcd");
});

test("escape_returns_to_what_had_the_keyboard_outside_the_terminals", async ({ page }) => {
  await createVault(page);
  const search_ = page.locator("button.search-button");
  await search_.focus();
  await page.keyboard.press("Meta+k");
  await expect(palette(page)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(palette(page)).toBeHidden();
  await expect(search_).toBeFocused();
});

test("a_preset_from_the_palette_on_a_new_tab_focuses_a_launcher_host", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await page.keyboard.press(KEYS.newTab);
  await expect(terminalTabs(page)).toHaveCount(1);

  await search(page, "2 columns");
  await box(page).press("Enter");
  await expect(palette(page)).toBeHidden();
  const launchers = panes(page).and(page.locator('[data-launcher="true"]'));
  await expect(launchers).toHaveCount(2);
  await expect(launchers.first().getByRole("button", { name: /^web-1/ })).toBeFocused();
});
