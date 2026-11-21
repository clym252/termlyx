import { expect, test } from "./fixtures";
import type { Page } from "@playwright/test";
import { KEYS, PASSWORD, connectHost, createVault, panes, seedHost, sessionIds } from "./helpers";

/** The card over one pane (the dimmed `.session-overlay` holds it). */
const card = (page: Page, paneIndex = 0) => panes(page).nth(paneIndex).locator(".session-card");
const drop = (page: Page, sessionId: string, reason = "connection reset by peer") =>
  page.evaluate(([id, why]) => window.__tetherMock.control.endSession(id, why), [sessionId, reason]);

/** A vault with one host, connected: its session is open and the pane shows no card. */
async function connected(page: Page): Promise<string> {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await expect(panes(page).first()).toHaveAttribute("data-status", "open");
  const [id] = await sessionIds(page);
  return id;
}

/** Makes the page report itself hidden or visible, as a backgrounded app does. */
function setVisibility(page: Page, state: "hidden" | "visible") {
  return page.evaluate((next) => {
    Object.defineProperty(document, "visibilityState", { get: () => next, configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
  }, state);
}

test("a_dropped_session_reconnects_by_itself", async ({ page }) => {
  const first = await connected(page);
  await drop(page, first);

  const c = card(page);
  await expect(c.getByRole("status")).toContainText("Reconnecting…");
  await expect(c).toContainText("attempt 1 of 5");
  await expect(c).toContainText(/next try in [01]s/);
  await expect(c.getByRole("button", { name: "Reconnect now" })).toBeVisible();
  await expect(c.getByRole("button", { name: "Cancel" })).toBeVisible();

  // The first try follows a second later and the pane is live again, on a new session.
  await expect(panes(page).first()).toHaveAttribute("data-status", "open", { timeout: 5000 });
  await expect(c).toHaveCount(0);
  const ids = await sessionIds(page);
  expect(ids).toHaveLength(1);
  expect(ids[0]).not.toBe(first);
});

test("reconnect_now_skips_the_wait_and_cancel_stops_retrying", async ({ page }) => {
  const first = await connected(page);
  await page.evaluate(() => {
    window.__tetherMock.control.failNextOpenSession("connection refused (os error 111)", "tcp");
  });
  await drop(page, first);
  await card(page).getByRole("button", { name: "Reconnect now" }).click();
  // The failed attempt leaves its reason on the step it stopped at, and the next try is scheduled.
  await expect(card(page)).toContainText("Reaching host — connection refused (os error 111)");
  await expect(card(page)).toContainText("attempt 2 of 5");

  await card(page).getByRole("button", { name: "Cancel" }).click();
  const lost = card(page).getByRole("alert");
  await expect(lost).toContainText("Connection lost");
  await expect(card(page).getByRole("button", { name: "Reconnect", exact: true })).toBeFocused();
  await expect(card(page).getByRole("button", { name: "Close" })).toBeVisible();

  // Nothing retries on its own after a cancel (the 2 s wait would have passed).
  await page.waitForTimeout(2500);
  await expect(lost).toContainText("Connection lost");
  expect(await sessionIds(page)).toHaveLength(0);

  // Reconnect opens a session again.
  await card(page).getByRole("button", { name: "Reconnect", exact: true }).click();
  await expect(panes(page).first()).toHaveAttribute("data-status", "open");
  expect(await sessionIds(page)).toHaveLength(1);
});

test("five_failed_attempts_give_up_with_connection_lost", async ({ page }) => {
  const first = await connected(page);
  await page.evaluate(() => {
    for (let i = 0; i < 5; i++) window.__tetherMock.control.failNextOpenSession("timed out", "tcp");
  });
  await drop(page, first);
  for (let attempt = 1; attempt <= 5; attempt++) {
    await expect(card(page)).toContainText(`attempt ${attempt} of 5`);
    await card(page).getByRole("button", { name: "Reconnect now" }).click();
  }
  await expect(card(page).getByRole("alert")).toContainText("Connection lost");
  await expect(card(page).getByRole("alert")).toContainText("Reaching host — timed out after 15 s");
  expect(await sessionIds(page)).toHaveLength(0);

  // Reconnect starts again (the sixth open succeeds).
  await card(page).getByRole("button", { name: "Reconnect", exact: true }).click();
  await expect(panes(page).first()).toHaveAttribute("data-status", "open");
});

test("a_drop_in_the_background_waits_for_a_tap", async ({ page }) => {
  const first = await connected(page);
  await setVisibility(page, "hidden");
  await drop(page, first);
  await setVisibility(page, "visible");

  const c = card(page);
  await expect(c.getByRole("alert")).toContainText("Connection lost");
  await expect(c.getByRole("alert")).toContainText("connection reset by peer");
  // No attempt on its own, however long the app is back in front.
  await page.waitForTimeout(3000);
  await expect(c.getByRole("alert")).toContainText("Connection lost");
  expect(await sessionIds(page)).toHaveLength(0);

  await c.getByRole("button", { name: "Reconnect", exact: true }).click();
  await expect(panes(page).first()).toHaveAttribute("data-status", "open");
  expect(await sessionIds(page)).toHaveLength(1);
});

test("a_shell_that_exited_is_never_reconnected", async ({ page }) => {
  const first = await connected(page);
  await page.evaluate((id) => window.__tetherMock.control.endSession(id, null, 0), first);
  await expect(card(page).getByRole("status")).toContainText("Session ended (exit status 0)");
  await page.waitForTimeout(2500);
  await expect(card(page).getByRole("status")).toContainText("Session ended (exit status 0)");
  expect(await sessionIds(page)).toHaveLength(0);
});

test("two_panes_on_one_host_both_come_back", async ({ page }) => {
  await connected(page);
  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  await expect(panes(page).nth(1)).toHaveAttribute("data-status", "open");
  const before = await sessionIds(page);

  // The shared connection goes: both sessions end together.
  for (const id of before) await drop(page, id);
  await expect(card(page, 0)).toContainText("Reconnecting…");
  await expect(card(page, 1)).toContainText("Reconnecting…");

  await expect(panes(page).nth(0)).toHaveAttribute("data-status", "open", { timeout: 5000 });
  await expect(panes(page).nth(1)).toHaveAttribute("data-status", "open", { timeout: 5000 });
  const after = await sessionIds(page);
  expect(after).toHaveLength(2);
  for (const id of after) expect(before).not.toContain(id);
});

test("a_changed_host_key_on_a_retry_asks_once_and_stops", async ({ page }) => {
  const first = await connected(page);
  // The server was rebuilt while connected: it presents another key than the known one.
  await page.evaluate(async () => {
    await window.__tetherMock.upsertEntity("known_host", {
      hostPattern: "web1.example",
      port: 22,
      algorithm: "ssh-ed25519",
      publicKey: "AAAAC3NzaC1lZDI1NTE5AAAAIOldKey",
    });
    window.__tetherMock.control.setServerKey("web1.example", 22, "ssh-ed25519", "AAAAC3NzaC1lZDI1NTE5AAAAINewKey");
  });
  await drop(page, first);

  const dialog = page.getByRole("dialog", { name: "Host key changed" });
  await expect(dialog).toBeVisible({ timeout: 5000 });
  await expect(card(page).getByRole("status")).toContainText("Checking host key — waiting for you");
  await dialog.getByRole("button", { name: "Reject" }).click();

  const lost = card(page).getByRole("alert");
  await expect(lost).toContainText("Connection lost");
  await expect(lost).toContainText("Checking host key — host key rejected");
  // No second attempt (and so no second dialog) follows.
  await page.waitForTimeout(3000);
  await expect(dialog).toHaveCount(0);
  expect(await page.evaluate(() => window.__tetherMock.control.pendingPrompts())).toEqual([]);
  expect(await sessionIds(page)).toHaveLength(0);
});

test("locking_during_the_wait_leaves_the_pane_to_the_user", async ({ page }) => {
  const first = await connected(page);
  await drop(page, first);
  await expect(card(page).getByRole("status")).toContainText("Reconnecting…");
  await page.evaluate(() => window.__tetherMock.vaultLock());
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
  // The wait would be over by now.
  await page.waitForTimeout(2500);
  expect(await sessionIds(page)).toHaveLength(0);

  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toHaveCount(0);
  // Back from the lock: no surprise reconnect, the card waits for a tap.
  await expect(card(page).getByRole("alert")).toContainText("Connection lost");
  await page.waitForTimeout(1500);
  expect(await sessionIds(page)).toHaveLength(0);
});

test("a_connection_that_drops_again_at_once_keeps_counting", async ({ page }) => {
  const first = await connected(page);
  await drop(page, first);
  await card(page).getByRole("button", { name: "Reconnect now" }).click();
  await expect(panes(page).first()).toHaveAttribute("data-status", "open");
  const [second] = await sessionIds(page);
  expect(second).not.toBe(first);

  // Dropped again within seconds of reopening: attempt 2, not a fresh attempt 1.
  await drop(page, second);
  await expect(card(page)).toContainText("attempt 2 of 5");
});

/** The `reattachTmux` of every host session opened so far, in order (`null`: none). */
const reattached = (page: Page) =>
  page.evaluate(() =>
    window.__tetherMock.control
      .openTargets()
      .map((t) => (t.kind === "host" ? (t.reattachTmux ?? null) : null)),
  );

test("a_tab_in_tmux_reattaches_to_its_session_after_a_drop", async ({ page }) => {
  const first = await connected(page);
  await page.evaluate((id) => window.__tetherMock.control.setTmux(id, "work"), first);
  // Going to the background asks the tab which tmux session it is in; the drop happens there.
  await setVisibility(page, "hidden");
  await drop(page, first);
  await setVisibility(page, "visible");

  await card(page).getByRole("button", { name: "Reconnect", exact: true }).click();
  await expect(panes(page).first()).toHaveAttribute("data-status", "open");
  // The first connect never reattaches; the reconnect does.
  expect(await reattached(page)).toEqual([null, "work"]);
});

test("two_tabs_on_one_host_each_reattach_to_their_own_tmux", async ({ page }) => {
  await connected(page);
  await page.keyboard.press(KEYS.splitRight);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  await expect(panes(page).nth(1)).toHaveAttribute("data-status", "open");
  const [inTmux, plain] = await sessionIds(page);
  await page.evaluate((id) => window.__tetherMock.control.setTmux(id, "work"), inTmux);
  await setVisibility(page, "hidden");
  await setVisibility(page, "visible");
  // Let the return to the front settle, so the drop is one in front (automatic reconnect).
  await page.waitForTimeout(2100);

  await drop(page, inTmux);
  await drop(page, plain);
  await expect(panes(page).nth(0)).toHaveAttribute("data-status", "open", { timeout: 5000 });
  await expect(panes(page).nth(1)).toHaveAttribute("data-status", "open", { timeout: 5000 });

  // One reconnect goes back to "work", the plain tab stays plain.
  const opened = await reattached(page);
  expect(opened.slice(0, 2)).toEqual([null, null]);
  expect(opened.slice(2).sort()).toEqual(["work", null].sort());
  const now = await sessionIds(page);
  const tmux = await page.evaluate(
    (ids) => Promise.all(ids.map((id) => window.__tetherMock.sessionTmux(id))),
    now,
  );
  expect(tmux.sort()).toEqual(["work", null].sort());
});
