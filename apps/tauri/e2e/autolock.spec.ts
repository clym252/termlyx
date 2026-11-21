import { expect, test } from "./fixtures";
import type { Page } from "@playwright/test";
import { PASSWORD, connectHost, createVault, goTo, panes, seedHost, sessionIds } from "./helpers";

const MINUTE = 60_000;

/** Makes the page report itself hidden or visible, as a backgrounded app does. */
function setVisibility(page: Page, state: "hidden" | "visible") {
  return page.evaluate((next) => {
    Object.defineProperty(document, "visibilityState", { get: () => next, configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
  }, state);
}

/** Time away while the app is suspended: the mock's clock moves, no timer runs. */
const away = (page: Page, ms: number) => page.evaluate((t) => window.__tetherMock.control.advanceClock(t), ms);
const opened = (page: Page) => page.evaluate(() => window.__tetherMock.control.openTargets().length);
const unlockHeading = (page: Page) => page.getByRole("heading", { name: "Unlock Termlyx" });
const card = (page: Page) => panes(page).first().locator(".session-card");

async function unlock(page: Page) {
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(unlockHeading(page)).toHaveCount(0);
}

/** A vault with one host; auto-lock set to `choice` in Settings → Security; the host connected. */
async function connectedWith(page: Page, choice: string | null): Promise<string> {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  if (choice) {
    await goTo(page, "Settings");
    await page.getByRole("combobox", { name: "Auto-lock" }).selectOption({ label: choice });
  }
  await connectHost(page, "web-1");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await expect(panes(page).first()).toHaveAttribute("data-status", "open");
  const [id] = await sessionIds(page);
  return id;
}

test("auto_lock_defaults_to_never", async ({ page }) => {
  await connectedWith(page, null);
  expect(await page.evaluate(() => window.__tetherMock.control.autoLockMinutes())).toBeNull();
  await goTo(page, "Settings");
  await expect(page.getByRole("combobox", { name: "Auto-lock" })).toHaveValue("never");
  await setVisibility(page, "hidden");
  await away(page, 24 * 60 * MINUTE);
  await setVisibility(page, "visible");
  await page.waitForTimeout(500);
  await expect(unlockHeading(page)).toHaveCount(0);
});

test("away_past_the_limit_locks_on_return_and_sessions_keep_running", async ({ page }) => {
  const session = await connectedWith(page, "After 5 minutes");
  expect(await page.evaluate(() => window.__tetherMock.control.autoLockMinutes())).toBe(5);

  // A short absence does not lock.
  await setVisibility(page, "hidden");
  await away(page, 4 * MINUTE);
  await setVisibility(page, "visible");
  await page.waitForTimeout(300);
  await expect(unlockHeading(page)).toHaveCount(0);

  await setVisibility(page, "hidden");
  await away(page, 6 * MINUTE);
  await setVisibility(page, "visible");
  await expect(unlockHeading(page)).toBeVisible();
  // The plain unlock screen: no "changed on another device" notice.
  await expect(page.getByText(/changed on another device/)).toHaveCount(0);
  // The session runs on under the lock (the layout stays mounted).
  expect(await sessionIds(page)).toEqual([session]);

  await unlock(page);
  await expect(panes(page).first()).toHaveAttribute("data-status", "open");
  expect(await sessionIds(page)).toEqual([session]);
});

// Review Focus 3: back from the background past the limit with a reconnect countdown overdue.
// The lock event is delivered late, so only the front check can stop the first tick's attempt.
test("a_pending_reconnect_does_not_run_under_an_auto_lock", async ({ page }) => {
  const first = await connectedWith(page, "After 5 minutes");
  await page.evaluate(() => window.__tetherMock.control.delayLockEvents(1000));
  expect(await opened(page)).toBe(1);

  await page.evaluate((id) => window.__tetherMock.control.endSession(id, "connection reset by peer"), first);
  await expect(card(page).getByRole("status")).toContainText("Reconnecting…");
  await setVisibility(page, "hidden");
  // Long enough that the 1 s wait is overdue on return.
  await page.waitForTimeout(1500);
  await away(page, 6 * MINUTE);
  await setVisibility(page, "visible");

  await expect(unlockHeading(page)).toBeVisible();
  // Past the first ticks and the late event: no attempt opened a session while locked.
  await page.waitForTimeout(2500);
  expect(await opened(page)).toBe(1);
  expect(await sessionIds(page)).toHaveLength(0);

  await unlock(page);
  // Back from the lock: no surprise reconnect, the card waits for a tap.
  const lost = card(page).getByRole("alert");
  await expect(lost).toContainText("Connection lost");
  await page.waitForTimeout(1500);
  expect(await opened(page)).toBe(1);

  await card(page).getByRole("button", { name: "Reconnect", exact: true }).click();
  await expect(panes(page).first()).toHaveAttribute("data-status", "open");
  expect(await opened(page)).toBe(2);
});

test("a_limit_the_backend_refuses_at_start_up_is_shown_and_can_be_retried", async ({ page }) => {
  // The device chose 5 minutes earlier; the backend refuses every start-up try (two per
  // start-up; the dev build's StrictMode runs Gate's start-up effect twice).
  await page.addInitScript(() => {
    localStorage.setItem("tether.auto-lock", "5");
    window.__tetherMockSeed = { autoLockFailures: 4 };
  });
  await createVault(page);
  expect(await page.evaluate(() => window.__tetherMock.control.autoLockMinutes())).toBeNull();

  await goTo(page, "Settings");
  await expect(page.getByRole("combobox", { name: "Auto-lock" })).toHaveValue("5");
  const alert = page.getByRole("alert").filter({ hasText: "Auto-lock is not on" });
  await expect(alert).toContainText("auto-lock is not ready");
  await alert.getByRole("button", { name: "Try again" }).click();
  await expect(alert).toHaveCount(0);
  expect(await page.evaluate(() => window.__tetherMock.control.autoLockMinutes())).toBe(5);
});
