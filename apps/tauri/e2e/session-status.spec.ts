import { expect, test } from "./fixtures";
import type { Page } from "@playwright/test";
import { connectHost, createVault, seedHost, sessionIds, terminalTabs } from "./helpers";

/** The card over the pane (the dimmed `.session-overlay` holds it). */
const status = (page: Page) => page.getByRole("tabpanel").locator(".session-card");
const steps = (page: Page) => status(page).getByRole("listitem");
/** Each step line's text and mark, in order. */
const stepLines = (page: Page) =>
  steps(page).evaluateAll((lis) => lis.map((li) => [li.textContent, li.getAttribute("data-mark")]));

/**
 * What xterm's WebGL renderer (the desktop app's) lays over the screen: a positioned canvas
 * with its own z-index. The mock-backed build draws with the DOM renderer, which has none.
 */
async function addRendererLayer(page: Page) {
  await page.evaluate(() => {
    const screen = document.querySelector(".terminal-pane .xterm-screen")!;
    const layer = document.createElement("canvas");
    layer.className = "xterm-link-layer";
    layer.style.cssText = "position:absolute;left:0;top:0;width:100%;height:100%;z-index:2";
    screen.appendChild(layer);
  });
}

/** Opens a terminal whose host key the user rejects, so it ends in "Could not connect". */
async function rejectedHostKey(page: Page) {
  await createVault(page);
  await seedHost(page, "db-1", "db1.example");
  await page.evaluate(async () => {
    await window.__tetherMock.upsertEntity("known_host", {
      hostPattern: "db1.example",
      port: 22,
      algorithm: "ssh-ed25519",
      publicKey: "AAAAC3NzaC1lZDI1NTE5AAAAIOldKey",
    });
    window.__tetherMock.control.setServerKey("db1.example", 22, "ssh-ed25519", "AAAAC3NzaC1lZDI1NTE5AAAAINewKey");
  });
  await connectHost(page, "db-1");
  const dialog = page.getByRole("dialog", { name: "Host key changed" });
  await dialog.getByRole("button", { name: "Reject" }).click();
  await expect(dialog).toBeHidden();
  await expect(status(page).getByRole("alert")).toContainText("host key rejected");
}

test("error_banner_buttons_work_over_the_terminal_renderer", async ({ page }) => {
  await rejectedHostKey(page);
  await addRendererLayer(page);

  // Retry asks about the key again.
  await status(page).getByRole("button", { name: "Retry" }).click({ timeout: 5000 });
  const dialog = page.getByRole("dialog", { name: "Host key changed" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Reject" }).click();
  await expect(status(page).getByRole("alert")).toContainText("host key rejected");

  // Close closes the pane (and with it the tab).
  await addRendererLayer(page);
  await status(page).getByRole("button", { name: "Close" }).click({ timeout: 5000 });
  await expect(terminalTabs(page)).toHaveCount(0);
});

test("error_banner_names_the_problem_and_takes_focus", async ({ page }) => {
  await createVault(page);
  await page.evaluate(() => window.__tetherMock.control.failNextOpenSession("connection refused (os error 111)"));
  await page.keyboard.press("Meta+Shift+L");
  const banner = status(page);
  await expect(banner.getByRole("alert")).toContainText("Could not connect");
  await expect(banner.getByRole("alert")).toContainText("connection refused (os error 111)");
  // A failure before any step was reported (a local shell) has no step line: the reason is its own line.
  await expect(banner.locator(".session-title")).toHaveText("Could not connect");
  await expect(banner.locator(".session-detail")).toHaveText("connection refused (os error 111)");
  await expect(steps(page)).toHaveCount(0);

  // The primary action has the focus; Tab reaches the other one.
  await expect(banner.getByRole("button", { name: "Retry" })).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(banner.getByRole("button", { name: "Close" })).toBeFocused();

  // Enter retries (the mock succeeds the second time) and the terminal takes the keys again.
  await banner.getByRole("button", { name: "Retry" }).focus();
  await page.keyboard.press("Enter");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await expect(banner).toHaveCount(0);
  await page.keyboard.type("ls\r");
  const [id] = await sessionIds(page);
  await expect.poll(() => page.evaluate((i) => window.__tetherMock.control.written(i).length, id)).toBeGreaterThan(0);
});

test("the_card_is_centred_over_a_dimmed_pane_and_names_the_host", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await page.evaluate(() => window.__tetherMock.control.holdNextOpenSession());
  await connectHost(page, "web-1");

  const card = status(page);
  await expect(card.getByRole("status")).toContainText("Connecting…");
  // The host: badge, label, and the login.
  await expect(card.locator(".host-badge")).toHaveText("W1");
  await expect(card.locator(".session-host-label")).toHaveText("web-1");
  await expect(card.locator(".session-host-address")).toHaveText("deploy@web1.example:22");

  // Reaching the host (the mock holds there until released).
  expect(await stepLines(page)).toEqual([
    ["Reaching host", "current"],
    ["Checking host key", "todo"],
    ["Signing in", "todo"],
    ["Opening shell", "todo"],
  ]);

  // Centred in the pane, and the pane behind it dimmed.
  const pane = (await page.getByRole("tabpanel").locator(".terminal-pane").boundingBox())!;
  const box = (await card.boundingBox())!;
  expect(Math.abs(box.x + box.width / 2 - (pane.x + pane.width / 2))).toBeLessThanOrEqual(2);
  expect(Math.abs(box.y + box.height / 2 - (pane.y + pane.height / 2))).toBeLessThanOrEqual(2);
  expect(box.width).toBeLessThanOrEqual(320);
  const veil = page.getByRole("tabpanel").locator(".session-overlay");
  const veilBox = (await veil.boundingBox())!;
  // The veil fills the pane inside its 1px border.
  for (const edge of ["x", "y", "width", "height"] as const) {
    expect(Math.abs(veilBox[edge] - pane[edge])).toBeLessThanOrEqual(2);
  }
  // The theme's background at 72%, whatever the theme.
  await expect(veil).toHaveCSS("background-color", /\/ 0\.72\)$/);
});

test("steps_follow_the_connection_and_the_card_goes_when_the_shell_opens", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  // Held while reaching the host; then the host key is unknown, so it waits for the user.
  await page.evaluate(() => {
    const { control } = window.__tetherMock;
    control.setServerKey("web1.example", 22, "ssh-ed25519", "AAAAC3NzaC1lZDI1NTE5AAAAINewKey");
    (window as unknown as { __held: { release(): void } }).__held = control.holdNextOpenSession();
  });
  await connectHost(page, "web-1");
  await expect.poll(() => stepLines(page)).toEqual([
    ["Reaching host", "current"],
    ["Checking host key", "todo"],
    ["Signing in", "todo"],
    ["Opening shell", "todo"],
  ]);
  await page.evaluate(() => (window as unknown as { __held: { release(): void } }).__held.release());
  const dialog = page.getByRole("dialog", { name: "Unknown host key" });
  await expect(dialog).toBeVisible();
  await expect(status(page).getByText("Checking host key — waiting for you")).toBeVisible();
  expect(await stepLines(page)).toEqual([
    ["Reaching host", "done"],
    ["Checking host key — waiting for you", "current"],
    ["Signing in", "todo"],
    ["Opening shell", "todo"],
  ]);
  await dialog.getByRole("button", { name: "Accept once" }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await expect(status(page)).toHaveCount(0);
});

test("a_failed_step_is_marked_with_its_reason_and_retry_connects_again", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await page.evaluate(() => window.__tetherMock.control.failNextOpenSession("timed out", "tcp"));
  await connectHost(page, "web-1");

  const card = status(page);
  await expect(card.getByRole("alert")).toContainText("Could not connect");
  expect(await stepLines(page)).toEqual([
    ["Reaching host — timed out after 15 s", "failed"],
    ["Checking host key", "todo"],
    ["Signing in", "todo"],
    ["Opening shell", "todo"],
  ]);
  await expect(card).toHaveCSS("border-top-color", "rgb(255, 122, 122)");
  await expect(card.getByRole("button", { name: "Retry" })).toBeFocused();

  // A refused login fails further along: the earlier steps are done.
  await page.evaluate(() => window.__tetherMock.control.failNextOpenSession("password rejected", "auth"));
  await card.getByRole("button", { name: "Retry" }).click();
  await expect(card.getByText("Signing in — password rejected")).toBeVisible();
  expect((await stepLines(page)).map(([, mark]) => mark)).toEqual(["done", "done", "failed", "todo"]);

  // The mock succeeds the third time.
  await card.getByRole("button", { name: "Retry" }).click();
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await expect(card).toHaveCount(0);
});

test("cancel_stops_connecting_and_closes_the_tab", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await page.evaluate(() => window.__tetherMock.control.holdNextOpenSession());
  await connectHost(page, "web-1");
  await expect(terminalTabs(page)).toHaveCount(1);
  await status(page).getByRole("button", { name: "Cancel" }).click();
  await expect(terminalTabs(page)).toHaveCount(0);
});

test("ended_session_shows_a_plain_card_with_close_only", async ({ page }) => {
  await createVault(page);
  await page.keyboard.press("Meta+Shift+L");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  const [id] = await sessionIds(page);
  await page.evaluate((i) => window.__tetherMock.control.endSession(i, null, 2), id);
  const card = status(page);
  await expect(card.getByRole("status")).toHaveText("Session ended (exit status 2)");
  await expect(steps(page)).toHaveCount(0);
  await expect(card.getByRole("button")).toHaveText(["Close"]);
  await expect(card).not.toHaveCSS("border-top-color", "rgb(255, 122, 122)");
});

test.describe("reduced motion", () => {
  test("the_card_neither_scales_in_nor_spins", async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await createVault(page);
    await seedHost(page, "web-1", "web1.example");
    await page.evaluate(() => window.__tetherMock.control.holdNextOpenSession());
    await connectHost(page, "web-1");
    await expect(status(page)).toBeVisible();
    expect(await stepLines(page)).toContainEqual(["Reaching host", "current"]);
    // No running animation on the card or on any step icon (or their pseudo-elements).
    const running = await page
      .getByRole("tabpanel")
      .locator(".session-overlay")
      .evaluate((el) => el.getAnimations({ subtree: true }).length);
    expect(running).toBe(0);
  });

  test("by_default_the_card_animates", async ({ page }) => {
    await createVault(page);
    await seedHost(page, "web-1", "web1.example");
    await page.evaluate(() => window.__tetherMock.control.holdNextOpenSession());
    await connectHost(page, "web-1");
    await expect(status(page)).toBeVisible();
    const running = await page
      .getByRole("tabpanel")
      .locator(".session-overlay")
      .evaluate((el) => el.getAnimations({ subtree: true }).length);
    expect(running).toBeGreaterThan(0);
  });
});

test.describe("small window", () => {
  test.use({ viewport: { width: 720, height: 480 } });

  test("long_error_wraps_inside_the_card_and_the_card_fits_the_pane", async ({ page }) => {
    await createVault(page);
    await page.keyboard.press("Meta+Shift+L");
    await expect.poll(() => sessionIds(page)).toHaveLength(1);
    const [id] = await sessionIds(page);
    const reason = `the remote end hung up: ${"unexpected-eof-while-reading-packet-".repeat(8)}`;
    // Dropped while the app is in the background, so it waits for a tap instead of reconnecting.
    const setVisibility = (next: string) =>
      page.evaluate((v) => {
        Object.defineProperty(document, "visibilityState", { get: () => v, configurable: true });
        document.dispatchEvent(new Event("visibilitychange"));
      }, next);
    await setVisibility("hidden");
    await page.evaluate(([i, r]) => window.__tetherMock.control.endSession(i, r), [id, reason]);
    await setVisibility("visible");

    const card = status(page);
    await expect(card.getByRole("alert")).toContainText("Connection lost");
    const pane = (await page.getByRole("tabpanel").locator(".terminal-pane").boundingBox())!;
    const box = (await card.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(pane.x);
    expect(box.x + box.width).toBeLessThanOrEqual(pane.x + pane.width);
    expect(box.y).toBeGreaterThanOrEqual(pane.y);
    expect(box.y + box.height).toBeLessThanOrEqual(pane.y + pane.height);
    // The reason wraps instead of running out of the card.
    const overflow = await card.evaluate((el) => {
      const detail = el.querySelector(".session-detail")!;
      return { detail: detail.scrollWidth - detail.clientWidth, card: el.scrollWidth - el.clientWidth };
    });
    expect(overflow).toEqual({ detail: 0, card: 0 });
    // Both actions are inside the window and the card.
    for (const name of ["Reconnect", "Close"]) {
      const b = (await card.getByRole("button", { name }).boundingBox())!;
      expect(b.x + b.width).toBeLessThanOrEqual(box.x + box.width);
      expect(b.y + b.height).toBeLessThanOrEqual(480);
    }
  });
});
