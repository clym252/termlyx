import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";
import { activePane, connectHost, createVault, emulateIos, hostRow, KEYS, panes, seedHost, sessionIds, written } from "./helpers";

async function seedSnippet(page: Page, label: string, text: string, run = false) {
  await page.evaluate(
    (input) => window.__tetherMock.upsertEntity("snippet", input),
    { label, text, run },
  );
}

const panel = (page: Page) => page.getByRole("complementary", { name: "Side panel" });
const screenWidth = async (page: Page) => (await activePane(page).locator(".xterm-screen").boundingBox())!.width;

async function connected(page: Page) {
  await createVault(page);
  await seedSnippet(page, "Disk usage", "df -h", true);
  await seedSnippet(page, "Two lines", "echo one\necho two");
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
}

test("side_panel_sends_a_snippet_and_the_terminal_refits", async ({ page }) => {
  await connected(page);
  const [id] = await sessionIds(page);
  const wide = await screenWidth(page);

  const keys = activePane(page).locator("textarea.xterm-helper-textarea");
  await page.getByRole("button", { name: "Side panel", exact: true }).click();
  await expect(panel(page)).toBeVisible();
  await expect(panel(page).getByRole("button", { name: "Snippets", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => screenWidth(page)).toBeLessThan(wide);
  // Opening the panel leaves the keys with the terminal.
  await expect(keys).toBeFocused();

  await panel(page).getByRole("button", { name: /^Disk usage/ }).click();
  await expect.poll(() => written(page, id)).toBe("df -h\r");
  // The panel stays; the terminal has the keys again.
  await expect(panel(page)).toBeVisible();
  await expect(activePane(page).locator("textarea.xterm-helper-textarea")).toBeFocused();

  await panel(page).getByRole("button", { name: "Close panel" }).click();
  await expect(panel(page)).toHaveCount(0);
  await expect(keys).toBeFocused();
  await expect.poll(() => screenWidth(page)).toBeGreaterThanOrEqual(wide - 1);
});

test("a_snippet_goes_to_every_pane_once_while_broadcasting", async ({ page }) => {
  await connected(page);
  await page.keyboard.press(KEYS.splitRight);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  for (const pane of await panes(page).all()) await expect(pane).toHaveAttribute("data-status", "open");
  await page.keyboard.press(KEYS.broadcast);
  const ids = await sessionIds(page);

  await page.getByRole("button", { name: "Side panel", exact: true }).click();
  await panel(page).getByRole("button", { name: /^Two lines/ }).click();
  for (const id of ids) {
    await expect.poll(() => written(page, id)).toContain("echo two");
  }
  await page.waitForTimeout(300);
  // One paste per pane (xterm turns the newline into a CR), and no Enter after it.
  for (const id of ids) expect(await written(page, id)).toBe("echo one\recho two");
});

test("themes_tab_switches_the_app_and_the_terminal", async ({ page }) => {
  await connected(page);
  await page.getByRole("button", { name: "Side panel", exact: true }).click();
  await panel(page).getByRole("button", { name: "Themes", exact: true }).click();
  await panel(page).getByRole("radio", { name: "Tokyo Night", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "tokyo-night");
  await expect(activePane(page)).toHaveAttribute("data-terminal-theme", "tokyo-night");
  await expect(activePane(page)).toHaveCSS("background-color", "rgb(26, 27, 38)");
  await expect(panel(page).getByRole("radio", { name: "Tokyo Night", exact: true })).toHaveAttribute("aria-checked", "true");
});

test("right_click_saves_the_selection_as_a_snippet", async ({ page }) => {
  await connected(page);
  const [id] = await sessionIds(page);
  await page.evaluate((id) => window.__tetherMock.control.output(id, new TextEncoder().encode("uptime\r\n")), id);
  const rows = activePane(page).locator(".xterm-rows");
  await expect(rows).toContainText("uptime");

  // No selection: copy and paste only.
  const box = (await activePane(page).locator(".xterm-screen").boundingBox())!;
  await page.mouse.click(box.x + box.width - 20, box.y + box.height - 20, { button: "right" });
  const menu = page.getByRole("menu", { name: "Terminal text" });
  await expect(menu.getByRole("menuitem")).toHaveText(["Copy", "Paste"]);
  await page.keyboard.press("Escape");

  // A word selected (double-click), then right-click on it.
  const row = (await rows.locator(":scope > div", { hasText: /^uptime/ }).first().boundingBox())!;
  const at = { x: row.x + 12, y: row.y + row.height / 2 };
  await page.mouse.dblclick(at.x, at.y);
  await page.mouse.click(at.x, at.y, { button: "right" });
  await expect(menu.getByRole("menuitem")).toHaveText(["Copy", "Paste", "Save as snippet"]);
  await menu.getByRole("menuitem", { name: "Save as snippet" }).click();
  const editor = page.getByRole("dialog", { name: "New snippet" });
  await expect(editor.getByLabel("Command")).toHaveValue("uptime");
  await expect(editor.getByLabel("Label")).toBeFocused();
  await expect(editor.getByLabel("Label")).toHaveValue("");
});

test("snippets_need_an_open_session", async ({ page }) => {
  await createVault(page);
  await seedSnippet(page, "Disk usage", "df -h");
  await page.keyboard.press(KEYS.newTab);
  await page.getByRole("button", { name: "Side panel", exact: true }).click();
  await expect(panel(page).getByText("Open a session to send snippets.")).toBeVisible();
  await expect(panel(page).getByRole("button", { name: /^Disk usage/ })).toBeDisabled();
});

test.describe("phone", () => {
  test.beforeEach(async ({ page }) => {
    await emulateIos(page);
  });

  test("terminal_text_is_clear_of_the_screen_edges", async ({ page }) => {
    await createVault(page);
    await seedHost(page, "web-1", "web1.example");
    await hostRow(page, "web-1").click();
    await expect(activePane(page)).toHaveAttribute("data-status", "open");
    const host = activePane(page).locator(".terminal-host");
    for (const side of ["padding-left", "padding-right"]) {
      expect(parseFloat(await host.evaluate((el, p) => getComputedStyle(el).getPropertyValue(p), side))).toBeGreaterThanOrEqual(8);
    }
    // The columns' leftover width is split between both sides, not all on the right.
    const h = (await host.boundingBox())!;
    const s = (await activePane(page).locator(".xterm-screen").boundingBox())!;
    const left = s.x - h.x;
    const right = h.x + h.width - (s.x + s.width);
    expect(Math.abs(left - right)).toBeLessThanOrEqual(1);
  });

  test("phone_panel_docks_under_the_terminal_one_thing_at_a_time", async ({ page }) => {
    await createVault(page);
    for (const label of ["Check storage", "Disk usage", "Upgrade"]) await seedSnippet(page, label, "df -h");
    await seedHost(page, "web-1", "web1.example");
    await hostRow(page, "web-1").click();
    await expect(activePane(page)).toHaveAttribute("data-status", "open");
    const menuItem = async (name: string) => {
      await page.getByRole("button", { name: "Session menu", exact: true }).click();
      await page.getByRole("menu", { name: "Session menu" }).getByRole("menuitem", { name }).click();
    };
    const inside = async (el: import("@playwright/test").Locator, box: import("@playwright/test").Locator) => {
      const [e, s] = [(await el.boundingBox())!, (await box.boundingBox())!];
      expect(e.y).toBeGreaterThanOrEqual(s.y);
      expect(e.y + e.height).toBeLessThanOrEqual(s.y + s.height);
    };
    // The terminal is not covered: it refits to end where the panel starts.
    const terminalAbove = async (panel: import("@playwright/test").Locator) => {
      const top = (await panel.boundingBox())!.y;
      await expect
        .poll(async () => {
          const screen = await activePane(page).locator(".xterm-screen").boundingBox();
          // A few px of slack: the fit leaves the host's padding under the last row.
          return !!screen && screen.y + screen.height <= top + 4;
        })
        .toBe(true);
    };

    // Snippets: only snippets, rows whole, Close beside the title.
    await menuItem("Snippets");
    const snippets = page.getByRole("complementary", { name: "Snippets" });
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(snippets.getByRole("button", { name: "Themes", exact: true })).toHaveCount(0);
    expect((await snippets.boundingBox())!.height).toBeLessThanOrEqual(852 / 2);
    await terminalAbove(snippets);
    for (const row of await snippets.getByRole("listitem").all()) await inside(row, snippets);
    const title = (await snippets.getByRole("heading", { name: "Snippets" }).boundingBox())!;
    const close = (await snippets.getByRole("button", { name: "Close" }).boundingBox())!;
    expect(Math.abs(close.y + close.height / 2 - (title.y + title.height / 2))).toBeLessThanOrEqual(2);
    await snippets.getByRole("button", { name: "Close" }).click();
    await expect(snippets).toBeHidden();

    // Themes: only themes, full-width swatches; the list scrolls inside the panel.
    await menuItem("Themes");
    const themes = page.getByRole("complementary", { name: "Themes" });
    await expect(themes.getByRole("button", { name: "Snippets", exact: true })).toHaveCount(0);
    await inside(themes.getByRole("radio", { name: "Termlyx Dark" }), themes);
    expect((await themes.getByRole("radio", { name: "Termlyx Dark" }).boundingBox())!.width).toBeGreaterThan(300);
    await terminalAbove(themes);

    // Tapping the terminal (the keyboard coming back) puts the panel away.
    await activePane(page).locator("textarea.xterm-helper-textarea").focus();
    await expect(themes).toBeHidden();
  });

  test("the_phone_panel_sends_a_snippet_and_stays_open", async ({ page }) => {
    await createVault(page);
    await seedSnippet(page, "Disk usage", "df -h", true);
    await seedHost(page, "web-1", "web1.example");
    await hostRow(page, "web-1").click();
    await expect(activePane(page)).toHaveAttribute("data-status", "open");
    const [id] = await sessionIds(page);

    await page.getByRole("button", { name: "Session menu", exact: true }).click();
    await page.getByRole("menu", { name: "Session menu" }).getByRole("menuitem", { name: "Snippets" }).click();
    const panel = page.getByRole("complementary", { name: "Snippets" });
    await panel.getByRole("button", { name: /^Disk usage/ }).click();
    await expect.poll(() => written(page, id)).toBe("df -h\r");
    await expect(panel).toBeVisible();
  });
});
