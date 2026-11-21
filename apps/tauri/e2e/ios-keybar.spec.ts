import { expect, test } from "./fixtures";
import type { Page } from "@playwright/test";
import {
  IPHONE_KEYBOARD_PX,
  activePane,
  connectHost,
  createVault,
  emulateIos,
  seedHost,
  sessionIds,
  softKeyboard,
  stubSoftKeyboard,
  written,
} from "./helpers";

test.beforeEach(async ({ page }, info) => {
  if (info.title.startsWith("ios_")) await emulateIos(page);
  else await stubSoftKeyboard(page);
});

const keyBar = (page: Page) => page.getByRole("toolbar", { name: "Terminal keys" });
const key = (page: Page, name: string) => keyBar(page).getByRole("button", { name, exact: true });

/** A vault with one host, connected, the terminal focused; returns the session id. */
async function connected(page: Page): Promise<string> {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await activePane(page).locator(".xterm").click();
  await expect(activePane(page).locator("textarea.xterm-helper-textarea")).toBeFocused();
  const [id] = await sessionIds(page);
  return id;
}

test("ios_key_bar_shows_only_with_the_software_keyboard", async ({ page }) => {
  await connected(page);
  await expect(keyBar(page)).toHaveCount(0);
  await softKeyboard(page, IPHONE_KEYBOARD_PX);
  await expect(keyBar(page)).toBeVisible();
  await softKeyboard(page, 0);
  await expect(keyBar(page)).toHaveCount(0);
});

test("ios_key_bar_keys_reach_the_session", async ({ page }) => {
  const id = await connected(page);
  await softKeyboard(page, IPHONE_KEYBOARD_PX);
  await expect(keyBar(page)).toBeVisible();

  await key(page, "Esc").click();
  await expect.poll(() => written(page, id)).toBe("\x1b");

  // Ctrl once, then a key typed on the keyboard: ^C, and ctrl is released.
  await key(page, "Ctrl").click();
  await expect(key(page, "Ctrl")).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.type("c");
  await expect.poll(() => written(page, id)).toBe("\x1b\x03");
  await expect(key(page, "Ctrl")).toHaveAttribute("aria-pressed", "false");
  await page.keyboard.type("c");
  await expect.poll(() => written(page, id)).toBe("\x1b\x03c");

  // The bar never takes the focus: the keyboard stays up.
  await key(page, "Up").click();
  await expect.poll(() => written(page, id)).toBe("\x1b\x03c\x1b[A");
  await expect(activePane(page).locator("textarea.xterm-helper-textarea")).toBeFocused();
});

test("ios_last_terminal_row_stays_above_the_key_bar", async ({ page }) => {
  await connected(page);
  await softKeyboard(page, IPHONE_KEYBOARD_PX);
  const bar = await keyBar(page).boundingBox();
  expect(bar).not.toBeNull();
  // The bar sits on the keyboard: its bottom is the visual viewport's.
  const visible = await page.evaluate(() => window.visualViewport!.height);
  expect(bar!.y + bar!.height).toBeLessThanOrEqual(visible + 0.5);
  expect(bar!.y + bar!.height).toBeGreaterThan(visible - 2);
  // The terminal refits above it (debounced): the last row ends at or above the bar's top.
  await expect
    .poll(async () => {
      const row = await activePane(page).locator(".xterm-rows > div").last().boundingBox();
      return row ? row.y + row.height <= bar!.y + 0.5 : false;
    })
    .toBe(true);
});

test("ios_snippets_key_opens_the_panel_in_place_of_the_keyboard", async ({ page }) => {
  const id = await connected(page);
  await softKeyboard(page, IPHONE_KEYBOARD_PX);
  await key(page, "Snippets").click();
  const sheet = page.getByRole("complementary", { name: "Snippets" });
  await expect(sheet).toBeVisible();
  // It takes the keyboard's place.
  await expect(activePane(page).locator("textarea.xterm-helper-textarea")).not.toBeFocused();
  await sheet.getByRole("button", { name: "Close" }).click();
  await expect(sheet).toBeHidden();
  expect(await written(page, id)).toBe("");
});

test("ios_hide_keyboard_key_closes_the_keyboard_and_a_tap_brings_it_back", async ({ page }) => {
  const id = await connected(page);
  const textarea = activePane(page).locator("textarea.xterm-helper-textarea");
  await softKeyboard(page, IPHONE_KEYBOARD_PX);
  await expect(key(page, "Hide keyboard")).toBeVisible();

  // The terminal lets go of the focus (iOS then lowers the keyboard), and the bar goes with it.
  await key(page, "Hide keyboard").click();
  await expect(textarea).not.toBeFocused();
  await expect(keyBar(page)).toHaveCount(0);
  await softKeyboard(page, 0);
  expect(await written(page, id)).toBe("");

  // A tap on the terminal takes the focus again; the keyboard and the bar come back.
  await activePane(page).locator(".xterm").click();
  await expect(textarea).toBeFocused();
  await softKeyboard(page, IPHONE_KEYBOARD_PX);
  await expect(keyBar(page)).toBeVisible();
});

/** Every key is at least 44 pt tall and the row fits the screen's width. */
async function expectKeysFit(page: Page) {
  const width = page.viewportSize()!.width;
  const boxes = await keyBar(page)
    .getByRole("button")
    .evaluateAll((els) => els.map((e) => e.getBoundingClientRect().toJSON() as DOMRect));
  expect(boxes.length).toBeGreaterThan(0);
  for (const b of boxes) {
    expect(b.height).toBeGreaterThanOrEqual(44);
    expect(b.left).toBeGreaterThanOrEqual(0);
    expect(b.right).toBeLessThanOrEqual(width + 0.5);
    expect(b.width).toBeGreaterThanOrEqual(30);
  }
}

test("ios_key_bar_shrinks_to_one_row_then_a_pill_and_remembers_it", async ({ page }) => {
  const id = await connected(page);
  const textarea = activePane(page).locator("textarea.xterm-helper-textarea");
  await softKeyboard(page, IPHONE_KEYBOARD_PX);
  const rows = keyBar(page).locator(".key-row");
  await expect(rows).toHaveCount(2);
  await expectKeysFit(page);

  await key(page, "One row").click();
  await expect(rows).toHaveCount(1);
  await expect(key(page, "Home")).toHaveCount(0);
  await expectKeysFit(page);
  await key(page, "Left").click();
  await expect.poll(() => written(page, id)).toBe("\x1b[D");
  await expect(textarea).toBeFocused();

  await key(page, "Hide key bar").click();
  await expect(keyBar(page)).toHaveCount(0);
  const show = page.getByRole("button", { name: "Show key bar" });
  await expect(show).toBeVisible();
  await expect(textarea).toBeFocused();
  expect(await page.evaluate(() => localStorage.getItem("tether.keyBar"))).toBe("hidden");
  // The terminal refits above the pill.
  const pill = (await show.locator(".key-bar-pill").boundingBox())!;
  await expect
    .poll(async () => {
      const row = await activePane(page).locator(".xterm-rows > div").last().boundingBox();
      return row ? row.y + row.height <= pill.y + 0.5 : false;
    })
    .toBe(true);

  // The keyboard goes and comes back: still the pill.
  await softKeyboard(page, 0);
  await expect(show).toHaveCount(0);
  await softKeyboard(page, IPHONE_KEYBOARD_PX);
  await expect(show).toBeVisible();

  await show.click();
  await expect(rows).toHaveCount(2);
  await expect(textarea).toBeFocused();
  expect(await page.evaluate(() => localStorage.getItem("tether.keyBar"))).toBe("two");
});

test("ios_key_bar_fits_a_narrow_phone", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 667 });
  await connected(page);
  await softKeyboard(page, 260);
  await expectKeysFit(page);
  await key(page, "One row").click();
  await expectKeysFit(page);
});

test("ios_ctrl_once_and_locked_look_different", async ({ page }) => {
  await connected(page);
  await softKeyboard(page, IPHONE_KEYBOARD_PX);
  const ctrl = key(page, "Ctrl");
  const look = () =>
    ctrl.evaluate((el) => {
      const s = getComputedStyle(el);
      const bar = getComputedStyle(el, "::after");
      return { bg: s.backgroundColor, color: s.color, bar: bar.content !== "none" && bar.content !== "" };
    });
  const off = await look();
  await ctrl.click();
  await expect(ctrl).toHaveAttribute("data-mod", "once");
  const once = await look();
  await ctrl.click();
  await expect(ctrl).toHaveAttribute("data-mod", "locked");
  const locked = await look();
  expect(new Set([off.bg, once.bg, locked.bg]).size).toBe(3);
  expect(once.bar).toBe(false);
  expect(locked.bar).toBe(true);
});

test("desktop_never_shows_the_key_bar", async ({ page }) => {
  const id = await connected(page);
  await softKeyboard(page, IPHONE_KEYBOARD_PX);
  await expect(activePane(page).locator(".terminal-host")).toBeVisible();
  await expect(keyBar(page)).toHaveCount(0);
  await page.keyboard.type("c");
  await expect.poll(() => written(page, id)).toBe("c");
});
