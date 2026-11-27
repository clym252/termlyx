import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";
import { PASSWORD, createVault, emulateIos, goTo, heldValues, holdFields, hostRow, seedHost } from "./helpers";

const SERVER = "https://sync.mock.test";
const FINGERPRINT = "sha256:0f1e2d3c4b5a69788796a5b4c3d2e1f00123456789abcdeffedcba9876543210";
const INVITE = "tether1-invite-demo";
const LINK = "tether1-link-demo";

const enableDialog = (page: Page) => page.getByRole("dialog", { name: "Connect to server" });
const syncSection = (page: Page) => page.getByRole("region", { name: "Sync" });
const dot = (page: Page) => page.getByTestId("sync-status");

/** A locked vault that syncs as alice, unlocked through the UI. */
async function seededUnlocked(page: Page, sync: object = {}) {
  await page.addInitScript(
    ({ password, sync }) => {
      window.__tetherMockSeed = { lockedVault: { password, quickUnlock: false }, sync };
    },
    { password: PASSWORD, sync },
  );
  await page.goto("/");
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
}

/** Code, confirm and username steps of the dialog, ending on the master password step. */
async function toPasswordStep(d: ReturnType<typeof enableDialog>, code: string, username = "alice") {
  await d.getByLabel("Invite or add-device code").fill(code);
  await d.getByRole("button", { name: "Continue" }).click();
  await d.getByRole("button", { name: "Continue" }).click();
  await d.getByLabel("Username").fill(username);
  await d.getByRole("button", { name: "Continue" }).click();
  await expect(d.getByLabel("Master password", { exact: true })).toBeFocused();
}

test("enable_sync_with_invite_existing_recovery", async ({ page }) => {
  const recoveryKey = await createVault(page);
  await goTo(page, "Settings");
  await expect(syncSection(page).getByRole("heading", { name: "Sync", level: 2 })).toBeVisible();
  // Off: no status dot.
  await expect(dot(page)).toHaveCount(0);
  await page.getByRole("button", { name: "Connect to server" }).click();

  const d = enableDialog(page);
  const code = d.getByLabel("Invite or add-device code");
  await expect(code).toBeFocused();
  // A bad code is explained in the step.
  await code.fill("nonsense");
  await d.getByRole("button", { name: "Continue" }).click();
  await expect(d.getByRole("alert")).toContainText("invalid code");

  await code.fill(`  ${INVITE}  `);
  await d.getByRole("button", { name: "Continue" }).click();
  // The server and its fingerprint, exactly as the backend returns it, to compare by eye.
  await expect(d).toContainText(`Server ${SERVER} · fingerprint ${FINGERPRINT}`);
  await expect(d.getByTestId("fingerprint")).toHaveText(FINGERPRINT);
  await expect(d).toContainText("Compare with termlyx-server fingerprint on your server");
  await d.getByRole("button", { name: "Continue" }).click();

  await expect(d.getByLabel("Username")).toBeFocused();
  await d.getByLabel("Username").fill("alice");
  await d.getByRole("button", { name: "Continue" }).click();

  const password = d.getByLabel("Master password", { exact: true });
  await expect(password).toBeFocused();
  await password.fill("not the password");
  await d.getByRole("button", { name: "Continue" }).click();

  await expect(d.getByRole("heading", { name: "Recovery key" })).toBeVisible();
  await d.getByRole("button", { name: "I have my recovery key" }).click();
  const key = d.getByLabel("Recovery key", { exact: true });
  await expect(key).toBeFocused();
  await key.fill(recoveryKey);
  await holdFields(password, key);
  await d.getByRole("button", { name: "Start syncing" }).click();
  // Wrong password: back in the password step, with the reason, and the field emptied.
  await expect(d.getByRole("alert")).toHaveText("Wrong master password.");
  await expect(password).toBeFocused();
  await expect(password).toHaveValue("");

  await password.fill(PASSWORD);
  await d.getByRole("button", { name: "Continue" }).click();
  await key.fill("WRONG-KEY");
  await d.getByRole("button", { name: "Start syncing" }).click();
  await expect(d.getByRole("alert")).toHaveText("That recovery key is wrong.");
  await expect(key).toBeFocused();
  await expect(key).toHaveValue("");

  await key.fill(recoveryKey);
  await d.getByRole("button", { name: "Start syncing" }).click();
  await expect(d.getByRole("status")).toContainText("Sync is on");
  // Nothing secret stays in the form.
  expect(await heldValues(page)).toEqual(["", ""]);
  await d.getByRole("button", { name: "Done" }).click();
  await expect(d).toBeHidden();

  const s = syncSection(page);
  await expect(s).toContainText(SERVER);
  await expect(s).toContainText("alice");
  await expect(s.getByTestId("sync-state")).toHaveText(/Synced/);
  await expect(s.getByRole("button", { name: "Sync now" })).toBeVisible();
  await expect(s.getByRole("button", { name: "Add device" })).toBeVisible();
  await expect(s.getByRole("button", { name: "Turn off sync on this device" })).toBeVisible();
  await expect(dot(page)).toHaveAttribute("data-state", "idle");
});

test("enable_sync_new_recovery_key_shown_once", async ({ page }) => {
  const oldKey = await createVault(page);
  await goTo(page, "Settings");
  await page.getByRole("button", { name: "Connect to server" }).click();
  const d = enableDialog(page);
  await toPasswordStep(d, INVITE);
  await d.getByLabel("Master password", { exact: true }).fill(PASSWORD);
  await d.getByRole("button", { name: "Continue" }).click();
  await d.getByRole("button", { name: "I lost it — create a new one" }).click();

  const keyDialog = page.getByRole("dialog", { name: "Your recovery key" });
  await expect(keyDialog).toBeVisible();
  const newKey = await keyDialog.getByLabel("Recovery key", { exact: true }).inputValue();
  expect(newKey).toMatch(/^MOCK-/);
  expect(newKey).not.toBe(oldKey);
  // It can only be closed with "I saved it" (Escape does nothing).
  await page.keyboard.press("Escape");
  await expect(keyDialog).toBeVisible();
  await keyDialog.getByRole("button", { name: "I saved it" }).click();
  await expect(keyDialog).toBeHidden();

  await expect(d.getByRole("status")).toContainText("Sync is on");
  await d.getByRole("button", { name: "Done" }).click();
  // The key is gone from the page for good.
  await expect(page.getByText(newKey)).toHaveCount(0);
  expect(await page.locator("input").evaluateAll((els, k) => els.filter((e) => (e as HTMLInputElement).value === k).length, newKey)).toBe(0);
});

/** After the next `enable` returns, the engine reports `status` (the server drops, or the upload drags on). */
async function afterEnable(page: Page, status: object) {
  await page.evaluate((status) => {
    const mock = window.__tetherMock;
    const original = mock.sync.enable.bind(mock.sync);
    mock.sync.enable = async (...args) => {
      const result = await original(...args);
      mock.control.setSyncStatus(status as never);
      return result;
    };
  }, status);
}

async function enableToTheEnd(page: Page) {
  const d = enableDialog(page);
  await page.getByRole("button", { name: "Connect to server" }).click();
  await toPasswordStep(d, INVITE);
  await d.getByLabel("Master password", { exact: true }).fill(PASSWORD);
  await d.getByRole("button", { name: "Continue" }).click();
  await d.getByRole("button", { name: "I lost it — create a new one" }).click();
  await page.getByRole("dialog", { name: "Your recovery key" }).getByRole("button", { name: "I saved it" }).click();
  return d;
}

test("enable_sync_offline_after_create_finishes", async ({ page }) => {
  await createVault(page);
  await goTo(page, "Settings");
  await afterEnable(page, { state: "offline", reason: "server unreachable" });
  const d = await enableToTheEnd(page);
  // No endless "Uploading": the dialog ends with the reason and a way out.
  await expect(d.getByRole("status")).toContainText("Sync is on, but the first sync did not finish");
  await expect(d.getByRole("alert")).toContainText("server unreachable");
  await expect(d.getByRole("alert")).toContainText("keeps retrying");
  await d.getByRole("button", { name: "Done" }).click();
  await expect(d).toBeHidden();
  await expect(syncSection(page)).toContainText("alice");
  await expect(dot(page)).toHaveAttribute("data-state", "offline");
});

test("enable_sync_upload_wait_can_be_left", async ({ page }) => {
  await createVault(page);
  await goTo(page, "Settings");
  await afterEnable(page, { state: "syncing", done: 1, total: 5 });
  const d = await enableToTheEnd(page);
  await expect(d.getByRole("status")).toContainText("Uploading your vault… 1 of 5");
  await d.getByRole("button", { name: "Close" }).click();
  await expect(d).toBeHidden();
  await expect(syncSection(page).getByTestId("sync-state")).toContainText("Syncing");
});

test("sync_errors_are_shown_in_their_step", async ({ page }) => {
  await createVault(page);
  await goTo(page, "Settings");
  await page.getByRole("button", { name: "Connect to server" }).click();
  const d = enableDialog(page);
  const code = d.getByLabel("Invite or add-device code");

  // A changed certificate is a strong warning.
  await page.evaluate(() =>
    window.__tetherMock.control.failNextSync("parseCode", "certificateChanged", "server certificate changed"),
  );
  await code.fill(INVITE);
  await d.getByRole("button", { name: "Continue" }).click();
  const warning = d.getByRole("alert");
  await expect(warning).toContainText("server certificate changed");
  await expect(warning).toContainText("Do not continue unless you rebuilt the server");
  await expect(warning).toHaveClass(/warning/);

  // A code from a newer app.
  await page.evaluate(() =>
    window.__tetherMock.control.failNextSync("parseCode", "updateRequired", "this code needs a newer Termlyx"),
  );
  await d.getByRole("button", { name: "Continue" }).click();
  await expect(d.getByRole("alert")).toContainText("this code needs a newer Termlyx");
  await expect(d.getByRole("alert")).toContainText("update Termlyx");

  // An unreachable server in the last step stays in that step, named with the reason; the form
  // keeps what was typed.
  await d.getByRole("button", { name: "Continue" }).click();
  await d.getByRole("button", { name: "Continue" }).click();
  await d.getByLabel("Username").fill("alice");
  await d.getByRole("button", { name: "Continue" }).click();
  await d.getByLabel("Master password", { exact: true }).fill(PASSWORD);
  await d.getByRole("button", { name: "Continue" }).click();
  await page.evaluate(() =>
    window.__tetherMock.control.failNextSync("enable", "unreachable", "server unreachable (timed out)"),
  );
  await d.getByRole("button", { name: "I lost it — create a new one" }).click();
  await expect(d.getByRole("alert")).toHaveText(
    "Can't reach sync.mock.test (timed out). Is the server running, and is this device on the same network?",
  );
  await expect(d.getByRole("heading", { name: "Recovery key" })).toBeVisible();

  // Escape closes the dialog.
  await page.keyboard.press("Escape");
  await expect(d).toBeHidden();
});

test("add_device_code_shows_expiry", async ({ page }) => {
  await page.clock.install();
  await seededUnlocked(page);
  await goTo(page, "Settings");
  await syncSection(page).getByRole("button", { name: "Add device" }).click();
  const d = page.getByRole("dialog", { name: "Add a device" });
  await expect(d.getByLabel("Add-device code", { exact: true })).toHaveValue("tether1-link-mock-1");
  await expect(d).toContainText("Expires in 10 minutes");
  await expect(d).toContainText("works once");
  await expect(d.getByRole("button", { name: "Copy code" })).toBeVisible();

  // The code expires.
  await page.clock.fastForward("11:00");
  await expect(d).toContainText("This code has expired");
  await d.getByRole("button", { name: "New code" }).click();
  await expect(d.getByLabel("Add-device code", { exact: true })).toHaveValue("tether1-link-mock-2");
  await expect(d).toContainText("Expires in 10 minutes");

  await page.keyboard.press("Escape");
  await expect(d).toBeHidden();
});

test("devices_rename_and_revoke_confirm", async ({ page }) => {
  await seededUnlocked(page);
  await page.evaluate(() => {
    window.__tetherMock.control.addDevice({ id: "dev-2", name: "Work laptop", platform: "linux" });
  });
  await goTo(page, "Settings");
  const list = syncSection(page).getByRole("list", { name: "Devices" });
  const here = list.getByRole("listitem").filter({ hasText: "This Mac" });
  const other = list.getByRole("listitem").filter({ hasText: "Work laptop" });
  await expect(here).toContainText("This device");
  // This device cannot be revoked here.
  await expect(here.getByRole("button", { name: /Revoke/ })).toHaveCount(0);
  await expect(other).toContainText("linux");
  await expect(other).toContainText("Last seen");

  // Rename.
  await other.getByRole("button", { name: "Rename Work laptop" }).click();
  const name = list.getByLabel("Device name");
  await expect(name).toBeFocused();
  await name.fill("Build box");
  await name.press("Escape");
  await expect(list.getByRole("listitem").filter({ hasText: "Work laptop" })).toHaveCount(1);
  await list.getByRole("button", { name: "Rename Work laptop" }).click();
  await list.getByLabel("Device name").fill("Build box");
  await list.getByRole("button", { name: "Save" }).click();
  const renamed = list.getByRole("listitem").filter({ hasText: "Build box" });
  await expect(renamed).toBeVisible();

  // Revoke asks first, with the exact copy.
  await renamed.getByRole("button", { name: "Revoke Build box" }).click();
  const confirm = page.getByRole("dialog", { name: "Revoke device" });
  await expect(confirm).toContainText(
    "Revoke Build box? It stops syncing right away. Its local copy stays locked behind your master password.",
  );
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(renamed).toBeVisible();

  await renamed.getByRole("button", { name: "Revoke Build box" }).click();
  await page.evaluate(() => window.__tetherMock.control.failNextSync("revokeDevice", "server", "the server said no"));
  await confirm.getByRole("button", { name: "Revoke", exact: true }).click();
  await expect(confirm.getByRole("alert")).toHaveText("the server said no");
  await confirm.getByRole("button", { name: "Revoke", exact: true }).click();
  await expect(confirm).toBeHidden();
  await expect(renamed).toHaveCount(0);
  await expect(here).toBeVisible();
});

test("turn_off_sync", async ({ page }) => {
  await seededUnlocked(page);
  await goTo(page, "Settings");
  await expect(dot(page)).toBeVisible();
  await syncSection(page).getByRole("button", { name: "Turn off sync on this device" }).click();
  const confirm = page.getByRole("dialog", { name: "Turn off sync" });
  await expect(confirm).toContainText("local vault");
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(confirm).toBeHidden();
  await expect(dot(page)).toBeVisible();

  await syncSection(page).getByRole("button", { name: "Turn off sync on this device" }).click();
  await page.evaluate(() => window.__tetherMock.control.failNextSync("turnOff", "storage", "could not clear the sign-in"));
  await confirm.getByRole("button", { name: "Turn off", exact: true }).click();
  await expect(confirm.getByRole("alert")).toHaveText("could not clear the sign-in");
  await confirm.getByRole("button", { name: "Turn off", exact: true }).click();
  await expect(confirm).toBeHidden();

  await expect(syncSection(page).getByRole("button", { name: "Connect to server" })).toBeVisible();
  await expect(syncSection(page).getByRole("button", { name: "Sync now" })).toHaveCount(0);
  await expect(dot(page)).toHaveCount(0);
});

test("join_synced_vault_from_first_run", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Create your vault" })).toBeVisible();
  await page.evaluate(() =>
    window.__tetherMock.control.setServerAccount({ username: "alice", password: "the synced password", recoveryKey: "RK-1" }),
  );
  await page.getByRole("button", { name: "Join a synced vault" }).click();
  await expect(page.getByRole("heading", { name: "Join a synced vault" })).toBeVisible();
  const code = page.getByLabel("Invite or add-device code");
  await expect(code).toBeFocused();

  // An invite creates an account; this screen joins one.
  await code.fill(INVITE);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByRole("alert")).toContainText("add-device code");

  await code.fill(LINK);
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByTestId("fingerprint")).toHaveText(FINGERPRINT);
  await page.getByRole("button", { name: "Continue" }).click();
  await page.getByLabel("Username").fill("alice");
  await page.getByRole("button", { name: "Continue" }).click();
  const password = page.getByLabel("Master password", { exact: true });
  await expect(password).toBeFocused();
  await password.fill("a wrong password");
  await holdFields(password);
  await page.getByRole("button", { name: "Download vault" }).click();
  await expect(page.getByRole("alert")).toHaveText("Wrong master password.");
  await expect(password).toBeFocused();
  await expect(password).toHaveValue("");

  // No vault here yet: no replace warning.
  await password.fill("the synced password");
  await page.getByRole("button", { name: "Download vault" }).click();
  await expect(page.getByRole("status")).toContainText("Your vault is synced");
  expect(await heldValues(page)).toEqual([""]);
  await page.getByRole("button", { name: "Open Termlyx" }).click();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  await expect(dot(page)).toHaveAttribute("data-state", "idle");
  await goTo(page, "Settings");
  await expect(syncSection(page)).toContainText("alice");
});

test("join_replace_existing_warns", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "10.0.0.5");
  await goTo(page, "Settings");
  await page.getByRole("button", { name: "Connect to server" }).click();
  const d = enableDialog(page);
  await toPasswordStep(d, LINK);
  await d.getByLabel("Master password", { exact: true }).fill(PASSWORD);
  await d.getByRole("button", { name: "Continue" }).click();

  // The host and its identity: two records.
  await expect(d).toContainText("This replaces this device's vault (2 records). A copy is kept as vault.sqlite.before-join-");
  await expect(d).not.toContainText("Sync is on");
  await d.getByRole("button", { name: "Cancel" }).click();
  await expect(d).toBeHidden();
  await expect(syncSection(page).getByRole("button", { name: "Connect to server" })).toBeVisible();
  await goTo(page, "Hosts");
  await expect(hostRow(page, "web-1")).toBeVisible();

  // Going through with it replaces the vault and says where the old one went.
  await goTo(page, "Settings");
  await page.getByRole("button", { name: "Connect to server" }).click();
  await toPasswordStep(d, LINK);
  await d.getByLabel("Master password", { exact: true }).fill(PASSWORD);
  await d.getByRole("button", { name: "Continue" }).click();
  await d.getByRole("button", { name: "Replace vault and join" }).click();
  await expect(d.getByRole("status")).toContainText("vault.sqlite.before-join-");
  await d.getByRole("button", { name: "Done" }).click();
  await expect(syncSection(page)).toContainText("alice");
  await goTo(page, "Hosts");
  await expect(hostRow(page, "web-1")).toHaveCount(0);
});

test("recover_on_a_new_device_needs_an_add_device_code", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() =>
    window.__tetherMock.control.setServerAccount({ username: "alice", password: "forgotten password", recoveryKey: "RK-1" }),
  );
  await page.getByRole("button", { name: "Join a synced vault" }).click();
  await page.getByRole("button", { name: "I forgot my master password" }).click();
  await page.getByLabel("Invite or add-device code").fill(INVITE);
  await page.getByRole("button", { name: "Continue" }).click();
  await page.getByRole("button", { name: "Continue" }).click();
  await page.getByLabel("Username").fill("alice");
  await page.getByRole("button", { name: "Continue" }).click();

  const recoveryKey = page.getByLabel("Recovery key", { exact: true });
  await expect(recoveryKey).toBeFocused();
  await recoveryKey.fill("WRONG");
  await page.getByLabel("New master password", { exact: true }).fill("a brand new password");
  await page.getByLabel("Confirm new master password").fill("a brand new password");
  await page.getByRole("button", { name: "Reset password" }).click();
  await expect(page.getByRole("alert")).toHaveText("That recovery key is wrong.");

  await recoveryKey.fill("RK-1");
  await page.getByLabel("New master password", { exact: true }).fill("a brand new password");
  await page.getByLabel("Confirm new master password").fill("a different one");
  await page.getByRole("button", { name: "Reset password" }).click();
  await expect(page.getByRole("alert")).toHaveText("The passwords do not match.");
  await page.getByLabel("Confirm new master password").fill("a brand new password");
  await page.getByRole("button", { name: "Reset password" }).click();

  // The account has a new password; this device still needs an add-device code to get the vault.
  await expect(page.getByRole("status")).toContainText("password was reset");
  await expect(page.getByRole("status")).toContainText("add-device code");
  const device = page.getByLabel("Add-device code", { exact: true });
  await expect(device).toBeFocused();
  await device.fill(LINK);
  await page.getByRole("button", { name: "Download vault" }).click();
  await expect(page.getByRole("status")).toContainText("Your vault is synced");
  await page.getByRole("button", { name: "Open Termlyx" }).click();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
});

test("change_password_flow", async ({ page }) => {
  await createVault(page);
  await goTo(page, "Settings");
  await page.getByRole("button", { name: "Change master password" }).click();
  const d = page.getByRole("dialog", { name: "Change master password" });
  const current = d.getByLabel("Current master password");
  const next = d.getByLabel("New master password", { exact: true });
  const confirm = d.getByLabel("Confirm new master password");
  await expect(current).toBeFocused();

  await current.fill(PASSWORD);
  await next.fill("short");
  await confirm.fill("short");
  await d.getByRole("button", { name: "Change password" }).click();
  await expect(d.getByRole("alert")).toContainText("at least 8 characters");

  await next.fill("a new master password");
  await confirm.fill("not the same");
  await d.getByRole("button", { name: "Change password" }).click();
  await expect(d.getByRole("alert")).toHaveText("The passwords do not match.");

  await current.fill("wrong current");
  await next.fill("a new master password");
  await confirm.fill("a new master password");
  await holdFields(current, next, confirm);
  await d.getByRole("button", { name: "Change password" }).click();
  await expect(d.getByRole("alert")).toHaveText("Wrong master password.");
  expect(await heldValues(page)).toEqual(["", "", ""]);

  await current.fill(PASSWORD);
  await next.fill("a new master password");
  await confirm.fill("a new master password");
  await d.getByRole("button", { name: "Change password" }).click();
  await expect(d.getByRole("status")).toContainText("Master password changed");
  expect(await heldValues(page)).toEqual(["", "", ""]);
  // Without sync there is nobody else to tell.
  await expect(d).not.toContainText("other devices");
  await d.getByRole("button", { name: "Done" }).click();
  await expect(d).toBeHidden();

  // The new password is the one that unlocks.
  await page.getByRole("button", { name: "Lock vault" }).click();
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("alert")).toHaveText("Wrong master password.");
  await page.getByLabel("Master password").fill("a new master password");
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeVisible();
});

test("change_password_with_sync_tells_other_devices", async ({ page }) => {
  await seededUnlocked(page, { status: { state: "offline", reason: "server unreachable" } });
  await goTo(page, "Settings");
  await page.getByRole("button", { name: "Change master password" }).click();
  const d = page.getByRole("dialog", { name: "Change master password" });
  await d.getByLabel("Current master password").fill(PASSWORD);
  await d.getByLabel("New master password", { exact: true }).fill("a new master password");
  await d.getByLabel("Confirm new master password").fill("a new master password");
  // With sync on the server hears of it first: offline is refused, plainly.
  await page.evaluate(() => window.__tetherMock.control.setSyncStatus({ state: "offline", reason: "server unreachable" }));
  await d.getByRole("button", { name: "Change password" }).click();
  await expect(d.getByRole("alert")).toContainText("server unreachable");
  await expect(d.getByRole("alert")).toContainText("Connect to the server");

  await page.evaluate(() => window.__tetherMock.control.setSyncStatus({ state: "idle", lastSync: Date.now() }));
  await d.getByLabel("Current master password").fill(PASSWORD);
  await d.getByLabel("New master password", { exact: true }).fill("a new master password");
  await d.getByLabel("Confirm new master password").fill("a new master password");
  await d.getByRole("button", { name: "Change password" }).click();
  await expect(d.getByRole("status")).toContainText("Master password changed");
  await expect(d).toContainText("other devices");
});

test("password_changed_elsewhere_message", async ({ page }) => {
  await seededUnlocked(page);
  // A manual lock gives no such message.
  await page.getByRole("button", { name: "Lock vault" }).click();
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
  await expect(page.getByText("Master password was changed on another device")).toHaveCount(0);
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();

  await page.evaluate(() => window.__tetherMock.control.changePasswordElsewhere("the newer password"));
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
  await expect(page.getByRole("status")).toHaveText("Master password was changed on another device. Enter the new one.");
  await expect(page.getByLabel("Master password")).toBeFocused();

  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("alert")).toHaveText("Wrong master password.");
  // Still explained after a wrong try.
  await expect(page.getByRole("status")).toContainText("Master password was changed on another device");

  await page.getByLabel("Master password").fill("the newer password");
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  await page.getByRole("button", { name: "Lock vault" }).click();
  await expect(page.getByText("Master password was changed on another device")).toHaveCount(0);
});

test("status_dot_states", async ({ page }) => {
  await seededUnlocked(page);
  const control = (fn: string, arg?: unknown) =>
    page.evaluate(([f, a]) => (window.__tetherMock.control as never as Record<string, (x: unknown) => void>)[f as string](a), [fn, arg]);

  await expect(dot(page)).toHaveAttribute("data-state", "idle");
  await expect(dot(page)).toContainText("Synced");
  await expect(dot(page)).toHaveAttribute("title", /Synced.*last sync/i);

  await control("setSyncStatus", { state: "syncing", done: 3, total: 10 });
  await expect(dot(page)).toHaveAttribute("data-state", "syncing");
  await expect(dot(page)).toContainText("Syncing");
  await expect(dot(page)).toHaveAttribute("title", /3 of 10/);

  await control("setSyncStatus", { state: "offline", reason: "server unreachable" });
  await expect(dot(page)).toHaveAttribute("data-state", "offline");
  await expect(dot(page)).toContainText("Offline");
  await expect(dot(page)).toHaveAttribute("title", /server unreachable/);

  await control("setSyncStatus", { state: "error", kind: "partial", message: "2 items could not be decrypted" });
  await expect(dot(page)).toHaveAttribute("data-state", "error");
  await expect(dot(page)).toHaveAttribute("title", /Some items didn't sync.*2 items could not be decrypted/);

  await control("setSyncStatus", { state: "error", kind: "certificateChanged", message: "server certificate changed" });
  await expect(dot(page)).toHaveAttribute("data-state", "error");
  await expect(dot(page)).toHaveAttribute("title", /server certificate changed/);

  // The Settings section tells the same story, with the advice, and "Sync now" works again once idle.
  await goTo(page, "Settings");
  await expect(syncSection(page).getByRole("alert")).toContainText("Do not continue unless you rebuilt the server");

  await control("setSyncStatus", { state: "error", kind: "partial", message: "2 items could not be decrypted" });
  await expect(syncSection(page).getByTestId("sync-state")).toContainText("Some items didn't sync");
  await expect(syncSection(page).getByTestId("sync-state")).toContainText("2 items could not be decrypted");

  await control("setSyncStatus", { state: "idle", lastSync: Date.now() });
  await expect(dot(page)).toHaveAttribute("data-state", "idle");
  const before = await page.evaluate(() => window.__tetherMock.control.syncNowCalls());
  await syncSection(page).getByRole("button", { name: "Sync now" }).click();
  await expect.poll(() => page.evaluate(() => window.__tetherMock.control.syncNowCalls())).toBe(before + 1);

  // Clicking the dot goes to the Sync settings.
  await goTo(page, "Hosts");
  await dot(page).click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeVisible();
});

test("sign_in_again_after_the_session_is_lost", async ({ page }) => {
  await seededUnlocked(page);
  const signInAgain = { state: "error", kind: "signInAgain", message: "not authorized; sign in again" };
  const setStatus = (s: object) => page.evaluate((st) => window.__tetherMock.control.setSyncStatus(st as never), s);

  // The dot says what to do, and opens the password prompt.
  await setStatus(signInAgain);
  await expect(dot(page)).toHaveAttribute("data-state", "error");
  await expect(dot(page)).toContainText("Sign in again");
  await dot(page).click();
  const d = page.getByRole("dialog", { name: "Sign in again" });
  await expect(d.getByLabel("Master password")).toBeFocused();
  await d.getByLabel("Master password").fill("not the password");
  await d.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(d.getByRole("alert")).toHaveText("Wrong master password.");
  await expect(d.getByLabel("Master password")).toHaveValue("");
  await d.getByLabel("Master password").fill(PASSWORD);
  await d.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(d).toBeHidden();
  await expect(dot(page)).toHaveAttribute("data-state", "idle");

  // Settings → Sync offers it too. A device the server no longer knows is told how to come back.
  await setStatus(signInAgain);
  await goTo(page, "Settings");
  await expect(syncSection(page).getByRole("alert")).toContainText("sign in again");
  await syncSection(page).getByRole("button", { name: "Sign in again" }).click();
  await page.evaluate(() =>
    window.__tetherMock.control.failNextSync(
      "signIn",
      "needsDeviceCode",
      "this device must be added to the account again with an add-device code",
    ),
  );
  await d.getByLabel("Master password").fill(PASSWORD);
  await d.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(d.getByRole("alert")).toContainText("add-device code");
  await expect(d.getByRole("alert")).toContainText("Turn off sync here");
  await d.getByRole("button", { name: "Cancel" }).click();
  await expect(d).toBeHidden();

  // Unlocking with the password signs in by itself.
  await page.getByRole("button", { name: "Lock vault" }).click();
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(dot(page)).toHaveAttribute("data-state", "idle");
  await goTo(page, "Settings");
  await expect(syncSection(page).getByTestId("sync-state")).toContainText("Synced");
  await expect(syncSection(page).getByRole("button", { name: "Sign in again" })).toHaveCount(0);
});

test("rejoin_the_same_vault_keeps_its_records", async ({ page }) => {
  await seededUnlocked(page);
  await goTo(page, "Settings");
  await syncSection(page).getByRole("button", { name: "Turn off sync on this device" }).click();
  await page.getByRole("dialog", { name: "Turn off sync" }).getByRole("button", { name: "Turn off", exact: true }).click();
  await seedHost(page, "edited while off", "10.0.0.7");

  // An add-device code for this very vault reconnects it: no replace warning, nothing lost.
  await goTo(page, "Settings");
  await page.getByRole("button", { name: "Connect to server" }).click();
  const d = enableDialog(page);
  await expect(d).toContainText("if it is this vault, it is reconnected and keeps its records");
  await toPasswordStep(d, LINK);
  await d.getByLabel("Master password", { exact: true }).fill(PASSWORD);
  await d.getByRole("button", { name: "Continue" }).click();
  await expect(d.getByRole("status")).toHaveText(
    "This vault is reconnected to sync. Its records are kept, and what changed while it was not syncing is uploaded now.",
  );
  await expect(d.getByText("This replaces this device's vault")).toBeHidden();
  await d.getByRole("button", { name: "Done" }).click();
  await expect(syncSection(page)).toContainText("alice");
  await goTo(page, "Hosts");
  await expect(hostRow(page, "edited while off")).toBeVisible();
});

test("a_failed_enable_still_shows_the_new_recovery_key", async ({ page }) => {
  await createVault(page);
  await goTo(page, "Settings");
  await page.getByRole("button", { name: "Connect to server" }).click();
  const d = enableDialog(page);
  await toPasswordStep(d, INVITE);
  await d.getByLabel("Master password", { exact: true }).fill(PASSWORD);
  await d.getByRole("button", { name: "Continue" }).click();
  await page.evaluate(() =>
    window.__tetherMock.control.failNextEnableAfterRegister("storage", "could not access stored credentials: injected"),
  );
  await d.getByRole("button", { name: "I lost it — create a new one" }).click();

  // The server already holds the new key: it is shown even though enabling failed.
  const keyDialog = page.getByRole("dialog", { name: "Your recovery key" });
  await expect(keyDialog).toBeVisible();
  const newKey = await keyDialog.getByLabel("Recovery key", { exact: true }).inputValue();
  expect(newKey).toMatch(/^MOCK-/);
  await keyDialog.getByRole("button", { name: "I saved it" }).click();
  await expect(keyDialog).toBeHidden();
  await expect(d.getByRole("alert")).toContainText("could not access stored credentials");
  await expect(page.getByText(newKey)).toHaveCount(0);
});

test("add_device_shows_a_qr_code_that_goes_with_the_code", async ({ page }) => {
  await seededUnlocked(page);
  await goTo(page, "Settings");
  await syncSection(page).getByRole("button", { name: "Add device" }).click();
  const d = page.getByRole("dialog", { name: "Add a device" });
  const qr = d.getByRole("img", { name: "QR code for this add-device code" });
  await expect(qr).toBeVisible();
  await expect(d.getByLabel("Add-device code", { exact: true })).toHaveValue("tether1-link-mock-1");
  // A picture of modules: the code is not in the page's markup apart from the field, and nothing is fetched.
  expect(await qr.evaluate((el) => el.outerHTML.includes("tether1-link"))).toBe(false);
  const before = await qr.innerHTML();
  await expect(d).toContainText("scan this QR code or paste the code");

  // Closing the dialog drops it; the next code is a new picture.
  await d.getByRole("button", { name: "Done" }).click();
  await expect(d).toBeHidden();
  await expect(page.getByRole("img", { name: "QR code for this add-device code" })).toHaveCount(0);
  await syncSection(page).getByRole("button", { name: "Add device" }).click();
  await expect(d.getByLabel("Add-device code", { exact: true })).toHaveValue("tether1-link-mock-2");
  expect(await d.getByRole("img", { name: "QR code for this add-device code" }).innerHTML()).not.toBe(before);
});

test("desktop_join_has_no_scan_button", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Join a synced vault" }).click();
  await expect(page.getByLabel("Invite or add-device code")).toBeVisible();
  await expect(page.getByRole("button", { name: "Scan QR code" })).toHaveCount(0);
});

test.describe("on iOS", () => {
  test.beforeEach(async ({ page }) => {
    await emulateIos(page);
  });

  test("scan_qr_code_fills_the_code_and_goes_on_to_the_fingerprint", async ({ page }) => {
    await page.goto("/");
    await page.getByRole("button", { name: "Join a synced vault" }).click();
    const code = page.getByLabel("Invite or add-device code");
    await expect(code).toBeVisible();
    await page.evaluate((text) => window.__tetherMock.control.setScan(text), LINK);

    await page.getByRole("button", { name: "Scan QR code" }).click();
    await expect(page.getByTestId("fingerprint")).toHaveText(FINGERPRINT);
    await expect(page.getByText(SERVER)).toBeVisible();
    // Back to the code step: the scanned code is in the field, as if pasted.
    await page.getByRole("button", { name: "Back" }).click();
    await expect(code).toHaveValue(LINK);
    expect(page.url()).not.toContain("tether1");
  });

  test("a_refused_camera_says_so_and_paste_still_works", async ({ page }) => {
    await page.goto("/");
    await page.getByRole("button", { name: "Join a synced vault" }).click();
    const code = page.getByLabel("Invite or add-device code");
    await page.getByRole("button", { name: "Scan QR code" }).click();
    await expect(page.getByRole("alert")).toContainText("camera");
    await expect(code).toHaveValue("");
    await code.fill(LINK);
    await page.getByRole("button", { name: "Continue" }).click();
    await expect(page.getByTestId("fingerprint")).toHaveText(FINGERPRINT);
  });

  test("cancel_scan_works_over_the_turn_on_sync_dialog", async ({ page }) => {
    // Settings hosts the form in a modal <dialog>: the camera prompt must be above it and tappable.
    await createVault(page);
    await goTo(page, "Settings");
    await page.getByRole("button", { name: "Connect to server" }).click();
    const d = enableDialog(page);
    await expect(d).toBeVisible();
    await page.evaluate(() => {
      const mock = window.__tetherMock as unknown as {
        scanQr: () => Promise<string | null>;
        cancelScan: () => Promise<void>;
      };
      let finish: (text: string | null) => void = () => {};
      mock.scanQr = () => new Promise((resolve) => (finish = resolve));
      mock.cancelScan = async () => finish(null);
    });
    await d.getByRole("button", { name: "Scan QR code" }).click();
    const scan = page.getByRole("dialog", { name: "Scan QR code" });
    await expect(scan).toBeVisible();
    await expect(page.locator("html")).toHaveAttribute("data-scanning", "true");
    // A real click: it fails if anything covers the button or the page under it is not inert.
    await scan.getByRole("button", { name: "Cancel scan" }).click();
    await expect(scan).toBeHidden();
    await expect(page.locator("html")).not.toHaveAttribute("data-scanning", "true");
    await expect(d.getByLabel("Invite or add-device code")).toHaveValue("");
    await expect(d.getByRole("alert")).toHaveCount(0);
    await expect(d.getByRole("button", { name: "Scan QR code" })).toBeEnabled();

    // Escape cancels a second scan the same way.
    await d.getByRole("button", { name: "Scan QR code" }).click();
    await expect(scan).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(scan).toBeHidden();
    await expect(d).toBeVisible();
  });
});


test("connect_to_server_with_setup_code", async ({ page }) => {
  const recoveryKey = await createVault(page);
  await goTo(page, "Settings");
  await page.getByRole("button", { name: "Connect to server" }).click();
  const d = enableDialog(page);
  await d.getByLabel("Setup, invite or add-device code").fill("TLX-ABCD-EFGH-JKMN-PQRS");
  await d.getByRole("button", { name: "Continue" }).click();
  // The code names no server: ask for its address, and show what the check found.
  await d.getByLabel("Server address").fill("127.0.0.1");
  await d.getByRole("button", { name: "Continue" }).click();
  await expect(d).toContainText(`Server https://127.0.0.1:8443 · fingerprint ${FINGERPRINT}`);
  await d.getByRole("button", { name: "Continue" }).click();
  await d.getByLabel("Username").fill("alice");
  await d.getByRole("button", { name: "Continue" }).click();
  await d.getByLabel("Master password", { exact: true }).fill(PASSWORD);
  await d.getByRole("button", { name: "Continue" }).click();
  await d.getByRole("button", { name: "I have my recovery key" }).click();
  await d.getByLabel("Recovery key", { exact: true }).fill(recoveryKey);
  await d.getByRole("button", { name: "Start syncing" }).click();
  await expect(d.getByRole("status")).toContainText("Sync is on");
  await d.getByRole("button", { name: "Done" }).click();
  await expect(syncSection(page)).toContainText("https://127.0.0.1:8443");
});
