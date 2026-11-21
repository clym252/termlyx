import { expect, test } from "./fixtures";
import type { Locator, Page } from "@playwright/test";
import {
  breadcrumb,
  createVault,
  editHost,
  folders,
  goTo,
  hostForm,
  hostCard,
  hostList,
  hostsBar,
  seedHost,
  sessionIds,
} from "./helpers";

// The smallest window the app allows (tauri.conf.json minWidth/minHeight).
test.use({ viewport: { width: 720, height: 480 } });

/** Whether the whole element is inside the 720×480 viewport. */
async function inViewport(locator: Locator) {
  await expect(locator).toBeVisible();
  const box = await locator.boundingBox();
  expect(box, "element has a box").not.toBeNull();
  const { x, y, width, height } = box!;
  expect(x).toBeGreaterThanOrEqual(0);
  expect(y).toBeGreaterThanOrEqual(0);
  expect(x + width).toBeLessThanOrEqual(720);
  expect(y + height).toBeLessThanOrEqual(480);
}

const scrollWidth = (page: Page) => page.evaluate(() => document.scrollingElement!.scrollWidth);

test("new_host_save_visible", async ({ page }) => {
  await createVault(page);
  await hostsBar(page).getByRole("button", { name: "New host" }).click();
  const form = hostForm(page);
  await inViewport(form.getByRole("button", { name: "Save host" }));
  await form.getByLabel("Label").fill("small-box");
  await form.getByLabel("Address").fill("10.0.0.9");
  await form.getByRole("button", { name: "Save host" }).click();
  // Saving closes the editor.
  await expect(form).toBeHidden();
  const labels = await page.evaluate(async () => {
    const hosts = (await window.__tetherMock.listEntities("host")) as { label: string }[];
    return hosts.map((h) => h.label);
  });
  expect(labels).toContain("small-box");
  expect(await scrollWidth(page)).toBeLessThanOrEqual(720);
});

test("edit_host_actions_visible", async ({ page }) => {
  await createVault(page);
  const label = "L".repeat(120);
  await seedHost(page, label, "fe80::1ff:fe23:4567:890a%en0");
  await hostCard(page, label).hover();
  await inViewport(hostList(page).getByRole("button", { name: `Connect to ${label}`, exact: true }));
  await inViewport(hostList(page).getByRole("button", { name: `Edit host ${label}`, exact: true }));
  expect(await scrollWidth(page)).toBeLessThanOrEqual(720);

  await editHost(page, label);
  const form = hostForm(page);
  await inViewport(form.getByRole("button", { name: "Save host" }));
  await inViewport(form.getByRole("button", { name: "Delete host" }));
  expect(await scrollWidth(page)).toBeLessThanOrEqual(720);
});

test("dialogs_actions_visible", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-01", "10.0.0.1");

  // New group.
  await hostsBar(page).getByRole("button", { name: "New group" }).click();
  let dialog = page.getByRole("dialog", { name: "New group" });
  await inViewport(dialog.getByRole("button", { name: "Save group" }));
  await dialog.getByLabel("Name").fill("Lab");
  await dialog.getByRole("button", { name: "Save group" }).click();
  await expect(dialog).toBeHidden();

  // Edit group and Delete group, from the group's own page.
  await folders(page).getByRole("button", { name: "Lab", exact: true }).click();
  for (const [item, title, primary] of [
    ["Edit group", "Edit group Lab", "Save group"],
    ["Delete group", "Delete group Lab", "Delete"],
  ]) {
    await breadcrumb(page).getByRole("button", { name: "Actions for Lab" }).click();
    await page.getByRole("menu", { name: "Lab" }).getByRole("menuitem", { name: item }).click();
    dialog = page.getByRole("dialog", { name: title });
    await inViewport(dialog.getByRole("button", { name: primary, exact: true }));
    await dialog.getByRole("button", { name: "Cancel" }).click();
    await expect(dialog).toBeHidden();
  }

  // Import SSH config…
  await breadcrumb(page).getByRole("button", { name: "All hosts" }).click();
  await hostsBar(page).getByRole("button", { name: "Import SSH config…" }).click();
  dialog = page.getByRole("dialog", { name: "Import SSH config" });
  await inViewport(dialog.getByRole("button", { name: "Use ~/.ssh/config" }));
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toBeHidden();

  // The delete-host confirm.
  await editHost(page, "web-01");
  await hostForm(page).getByRole("button", { name: "Delete host" }).click();
  dialog = page.getByRole("dialog", { name: "Delete host web-01" });
  await inViewport(dialog.getByRole("button", { name: "Delete", exact: true }));
});

test("settings_last_control_reachable", async ({ page }) => {
  await createVault(page);
  await goTo(page, "Settings");
  const last = page.locator("section.settings-page").getByRole("button").last();
  await last.scrollIntoViewIfNeeded();
  await inViewport(last);
  await expect(last).toBeEnabled();
});

test("editor_escape_closes", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-01", "10.0.0.1");
  await editHost(page, "web-01");
  const panel = page.getByRole("complementary", { name: "Host editor" });
  await expect(panel).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(panel).toBeHidden();
  await expect(hostList(page).getByRole("button", { name: "Edit host web-01", exact: true })).toBeFocused();

  // A new host's editor hands focus back to New host.
  await hostsBar(page).getByRole("button", { name: "New host" }).click();
  await expect(hostForm(page).getByLabel("Label")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(panel).toBeHidden();
  await expect(hostsBar(page).getByRole("button", { name: "New host" })).toBeFocused();
});

test("escape_in_a_dialog_leaves_the_panel_open", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-01", "10.0.0.1");
  await editHost(page, "web-01");
  await hostForm(page).getByRole("button", { name: "Delete host" }).click();
  const dialog = page.getByRole("dialog", { name: "Delete host web-01" });
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("complementary", { name: "Host editor" })).toBeVisible();
});

test.describe("grid stays usable beside the panel", () => {
  test("wide_window_panel_does_not_cover_cards", async ({ page }) => {
    await page.setViewportSize({ width: 1000, height: 640 });
    await createVault(page);
    await seedHost(page, "web-01", "10.0.0.1");
    await seedHost(page, "db-01", "10.0.0.2");
    await editHost(page, "web-01");
    const panel = page.getByRole("complementary", { name: "Host editor" });
    const panelBox = (await panel.boundingBox())!;
    expect(panelBox.width).toBeGreaterThanOrEqual(320);
    expect(panelBox.width).toBeLessThanOrEqual(420);

    // Another host's card is clear of the panel, and its Connect works.
    await hostCard(page, "db-01").hover();
    const connect = hostList(page).getByRole("button", { name: "Connect to db-01", exact: true });
    await expect(connect).toBeVisible();
    const box = (await connect.boundingBox())!;
    expect(box.x + box.width).toBeLessThanOrEqual(panelBox.x);
    await connect.click();
    await expect.poll(() => sessionIds(page)).toHaveLength(1);
  });

  test("just_above_the_breakpoint_no_card_is_clipped", async ({ page }) => {
    await page.setViewportSize({ width: 770, height: 640 });
    await createVault(page);
    await seedHost(page, "web-01", "10.0.0.1");
    await seedHost(page, "L".repeat(120), "fe80::1ff:fe23:4567:890a%en0");
    await editHost(page, "web-01");
    const pane = page.locator(".host-list-pane");
    const [scroll, client] = await pane.evaluate((el) => [el.scrollWidth, el.clientWidth]);
    expect(scroll).toBeLessThanOrEqual(client);
    const panelBox = (await page.getByRole("complementary", { name: "Host editor" }).boundingBox())!;
    for (const label of ["web-01", "L".repeat(120)]) {
      await hostCard(page, label).hover();
      const box = (await hostList(page).getByRole("button", { name: `Connect to ${label}`, exact: true }).boundingBox())!;
      expect(box.x + box.width).toBeLessThanOrEqual(panelBox.x);
    }
  });
});
