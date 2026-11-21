import { expect, test } from "./fixtures";
import type { Page } from "@playwright/test";
import { PASSWORD, connectHost, createVault, panes, seedHost, sessionIds } from "./helpers";
import type { MockSeed } from "../src/api/mock";

const UPDATE = { version: "0.2.0", notes: "Faster SFTP listings.", date: "2026-11-20T10:00:00Z" };

const notice = (page: Page) => page.getByRole("region", { name: "Update available" });
const checks = (page: Page) => page.evaluate(() => window.__tetherMock.control.updateChecks());
const installs = (page: Page) => page.evaluate(() => window.__tetherMock.control.updateInstalls());

/** Seeds the mock (an update, a locked vault) before the app's first render. */
async function seed(page: Page, s: MockSeed) {
  await page.addInitScript((value) => {
    window.__tetherMockSeed = value;
  }, s);
}

async function unlock(page: Page) {
  await page.goto("/");
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
}

test("update notice after unlock", async ({ page }) => {
  await seed(page, { lockedVault: { password: PASSWORD, quickUnlock: false }, update: UPDATE });
  await unlock(page);
  const n = notice(page);
  // Announced by screen readers.
  await expect(n.getByRole("status")).toHaveText("Version 0.2.0 is available");
  // Non-blocking: the page behind it still works.
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await n.getByRole("button", { name: "View changes" }).click();
  const changes = page.getByRole("dialog", { name: "What's new in 0.2.0" });
  await expect(changes).toContainText("Faster SFTP listings.");
  await changes.getByRole("button", { name: "Close" }).click();
  await expect(n.getByRole("button", { name: "Update now" })).toBeVisible();
  await n.getByRole("button", { name: "Later" }).click();
  await expect(n).toHaveCount(0);

  // For the rest of the session: locking and unlocking neither checks again nor shows it.
  await page.getByRole("button", { name: "Lock vault" }).click();
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  await page.waitForTimeout(300);
  await expect(n).toHaveCount(0);
  expect(await checks(page)).toBe(1);
  expect(await installs(page)).toBe(0);
});

test("update check failure stays quiet", async ({ page }) => {
  await seed(page, { lockedVault: { password: PASSWORD, quickUnlock: false }, update: "fail" });
  await unlock(page);
  await expect.poll(() => checks(page)).toBe(1);
  await page.waitForTimeout(300);
  await expect(notice(page)).toHaveCount(0);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("update asks before closing sessions", async ({ page }) => {
  await seed(page, { update: UPDATE });
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await expect(panes(page).first()).toHaveAttribute("data-status", "open");

  await notice(page).getByRole("button", { name: "Update now" }).click();
  const confirm = page.getByRole("dialog", { name: "Install update" });
  await expect(confirm).toContainText("1 open session will close");
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(confirm).toHaveCount(0);
  expect(await installs(page)).toBe(0);
  expect(await sessionIds(page)).toHaveLength(1);

  await notice(page).getByRole("button", { name: "Update now" }).click();
  await confirm.getByRole("button", { name: "Update and restart" }).click();
  await expect.poll(() => installs(page)).toBe(1);
  await expect(notice(page)).toContainText("100%");
});

test("no check when auto-check is off", async ({ page }) => {
  await seed(page, { lockedVault: { password: PASSWORD, quickUnlock: false }, update: UPDATE });
  await page.addInitScript(() => localStorage.setItem("tether.update-check", "false"));
  await unlock(page);
  await page.waitForTimeout(300);
  expect(await checks(page)).toBe(0);
  await expect(notice(page)).toHaveCount(0);
});
