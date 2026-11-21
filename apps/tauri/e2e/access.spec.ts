import { expect, test } from "./fixtures";
import { PASSWORD, createVault, generateKey, goTo, hostRow } from "./helpers";

const QUICK = "Unlock with system keychain";

test("quick_unlock_enable_and_use", async ({ page }) => {
  await createVault(page);
  await goTo(page, "Settings");
  const toggle = page.getByRole("checkbox", { name: QUICK });
  await expect(toggle).not.toBeChecked();

  // A keychain failure is shown, and the toggle stays off.
  await page.evaluate(() => window.__tetherMock.control.failQuickUnlock("enable", "keychain access was denied"));
  await toggle.click();
  await expect(page.getByRole("alert")).toContainText("keychain access was denied");
  await expect(toggle).not.toBeChecked();

  await toggle.check();
  await expect(toggle).toBeChecked();
  await expect(page.getByRole("alert")).toHaveCount(0);

  await page.getByRole("button", { name: "Lock vault" }).click();
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
  // The button is offered; a failed attempt falls back to the password form with the reason.
  await page.evaluate(() => window.__tetherMock.control.failQuickUnlock("unlock", "the keychain is locked"));
  await page.getByRole("button", { name: "Quick unlock" }).click();
  await expect(page.getByRole("alert")).toContainText("the keychain is locked");
  await expect(page.getByLabel("Master password")).toBeVisible();
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();

  await page.getByRole("button", { name: "Quick unlock" }).click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeVisible();

  // Turning it off removes the button.
  await expect(page.getByRole("checkbox", { name: QUICK })).toBeChecked();
  await page.getByRole("checkbox", { name: QUICK }).uncheck();
  await expect(page.getByRole("checkbox", { name: QUICK })).not.toBeChecked();
  await page.getByRole("button", { name: "Lock vault" }).click();
  await expect(page.getByRole("button", { name: "Quick unlock" })).toHaveCount(0);
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeVisible();
});

test("touch_id_label_and_auto_prompt", async ({ page }) => {
  // A locked vault with Touch ID unlock on, and the first Touch ID declined.
  await page.addInitScript((password) => {
    window.__tetherMockSeed = {
      quickUnlockKind: "touchId",
      lockedVault: { password, quickUnlock: true },
      quickUnlockFailure: { message: "Touch ID was not confirmed", code: "presenceDenied" },
    };
  }, PASSWORD);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
  // The window's first unlock screen asks Touch ID by itself, once.
  await expect.poll(() => page.evaluate(() => window.__tetherMock.control.quickUnlockCalls())).toBe(1);
  // Declining is not an error: the password field is ready.
  await expect(page.getByRole("alert")).toHaveCount(0);
  await expect(page.getByLabel("Master password")).toBeFocused();

  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await goTo(page, "Settings");
  await expect(page.getByRole("checkbox", { name: "Unlock with Touch ID" })).toBeChecked();

  // Locking by hand does not prompt again; the button does.
  await page.getByRole("button", { name: "Lock vault" }).click();
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
  expect(await page.evaluate(() => window.__tetherMock.control.quickUnlockCalls())).toBe(1);

  // Changed fingers: the reason is shown and the button goes away.
  await page.evaluate(() =>
    window.__tetherMock.control.failQuickUnlock(
      "unlock",
      "Touch ID fingers changed; quick unlock was turned off",
      "biometryChanged",
    ),
  );
  await page.getByRole("button", { name: "Unlock with Touch ID" }).click();
  await expect(page.getByRole("alert")).toContainText("fingers changed");
  await expect(page.getByRole("button", { name: "Unlock with Touch ID" })).toHaveCount(0);
  expect(await page.evaluate(() => window.__tetherMock.control.quickUnlockCalls())).toBe(2);
});

test("touch_id_unavailable_auto_attempt_is_quiet", async ({ page }) => {
  // A Mac without a sensor (or a closed lid) and Touch ID unlock left on.
  await page.addInitScript((password) => {
    window.__tetherMockSeed = {
      quickUnlockKind: "touchId",
      lockedVault: { password, quickUnlock: true },
      quickUnlockFailure: { message: "Touch ID is not available: no biometry", code: "biometryUnavailable" },
    };
  }, PASSWORD);
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__tetherMock.control.quickUnlockCalls())).toBe(1);
  // The automatic attempt does not greet every launch with an error.
  await expect(page.getByRole("alert")).toHaveCount(0);
  await expect(page.getByLabel("Master password")).toBeFocused();
  // Asking by hand says why.
  await page.evaluate(() =>
    window.__tetherMock.control.failQuickUnlock("unlock", "Touch ID is not available: no biometry", "biometryUnavailable"),
  );
  await page.getByRole("button", { name: "Unlock with Touch ID" }).click();
  await expect(page.getByRole("alert")).toContainText("not available");
});

test("import_ssh_config_reports_what_happened", async ({ page }) => {
  await createVault(page);
  await page.evaluate(() => {
    window.__tetherMock.control.setFile(
      "~/.ssh/config",
      [
        "Host alpha",
        "  HostName 10.1.1.1",
        "  User deploy",
        "  Port 2200",
        "Host beta",
        "  HostName 10.1.1.2",
        "Host *.internal",
        "  User wild",
        "Host jumped",
        "  ProxyJump alpha",
        "",
      ].join("\n"),
    );
    window.__tetherMock.control.setFile("/work/other_config", "Host gamma\n  HostName 10.2.2.2\n");
  });

  await page.getByRole("button", { name: "Import SSH config…" }).click();
  const dialog = page.getByRole("dialog", { name: "Import SSH config" });
  await dialog.getByRole("button", { name: "Use ~/.ssh/config" }).click();

  const report = dialog.getByRole("region", { name: "Import result" });
  await expect(report).toContainText("Hosts added: 2");
  await expect(report).toContainText("Identities added: 1");
  await expect(report).toContainText("Keys added: 0");
  const skipped = report.getByRole("list", { name: "Skipped" });
  await expect(skipped).toContainText("jumped");
  await expect(skipped).toContainText("ProxyJump");
  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(dialog).toBeHidden();
  await expect(hostRow(page, "alpha")).toBeVisible();
  await expect(hostRow(page, "beta")).toBeVisible();

  // Another file through the picker; a cancelled picker does nothing.
  await page.getByRole("button", { name: "Import SSH config…" }).click();
  const again = page.getByRole("dialog", { name: "Import SSH config" });
  await again.getByRole("button", { name: "Choose file…" }).click();
  await expect(again.getByRole("region", { name: "Import result" })).toHaveCount(0);
  await page.evaluate(() => window.__tetherMock.control.setPick("/work/other_config"));
  await again.getByRole("button", { name: "Choose file…" }).click();
  await expect(again.getByRole("region", { name: "Import result" })).toContainText("Hosts added: 1");
  await again.getByRole("button", { name: "Done" }).click();
  await expect(hostRow(page, "gamma")).toBeVisible();

  // A file that cannot be read is an error, not a report.
  await page.getByRole("button", { name: "Import SSH config…" }).click();
  const missing = page.getByRole("dialog", { name: "Import SSH config" });
  await page.evaluate(() => window.__tetherMock.control.setPick("/work/missing"));
  await missing.getByRole("button", { name: "Choose file…" }).click();
  await expect(missing.getByRole("alert")).toContainText("could not read /work/missing");
});

test("import_ssh_config_lists_key_failures_needing_a_passphrase", async ({ page }) => {
  await createVault(page);
  await goTo(page, "Hosts");
  // The mock's import never reads key files; the report's shape is what is checked, so build one.
  await page.evaluate(() => {
    const mock = window.__tetherMock;
    const original = mock.importSshConfig.bind(mock);
    mock.importSshConfig = async (path) => ({
      ...(await original(path)),
      keysAdded: 1,
      keyFailures: [
        { path: "/home/me/.ssh/id_work", needsPassphrase: true, message: "key is encrypted and needs a passphrase" },
        { path: "/home/me/.ssh/id_old", needsPassphrase: false, message: "could not parse key" },
      ],
    });
    mock.control.setFile("~/.ssh/config", "Host alpha\n  HostName 10.1.1.1\n");
  });
  await page.getByRole("button", { name: "Import SSH config…" }).click();
  const dialog = page.getByRole("dialog", { name: "Import SSH config" });
  await dialog.getByRole("button", { name: "Use ~/.ssh/config" }).click();
  const failures = dialog.getByRole("list", { name: "Keys that could not be imported" });
  const work = failures.getByRole("listitem").filter({ hasText: "id_work" });
  await expect(work).toContainText("needs a passphrase");
  await expect(work).toContainText("Keychain");
  await expect(failures.getByRole("listitem").filter({ hasText: "id_old" })).toContainText("could not parse key");
});

test("export_private_key_asks_first", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await createVault(page);
  await generateKey(page, "laptop");
  await page.evaluate(() => navigator.clipboard.writeText("untouched"));

  await page.getByRole("button", { name: "Edit key laptop" }).click();
  const editor = page.getByRole("dialog", { name: "Edit key laptop" });
  await expect(editor.getByLabel("Public key")).toHaveValue(/^ssh-ed25519 /);
  await editor.getByRole("button", { name: "Export private key…" }).click();

  // An explicit confirmation names the risk; cancelling leaves the clipboard alone.
  const confirm = page.getByRole("dialog", { name: "Export private key" });
  await expect(confirm).toContainText("who has it can sign in");
  await confirm.getByRole("button", { name: "Cancel" }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("untouched");

  await editor.getByRole("button", { name: "Export private key…" }).click();
  await page.getByRole("dialog", { name: "Export private key" }).getByRole("button", { name: "Copy private key" }).click();
  await expect(editor.getByRole("status")).toContainText("Private key copied");
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toContain("BEGIN OPENSSH PRIVATE KEY");
  // The page never shows it.
  expect(await page.content()).not.toContain("BEGIN OPENSSH PRIVATE KEY");
  expect(await page.evaluate(() => document.body.innerText)).not.toContain("BEGIN OPENSSH PRIVATE KEY");

  // Renaming a key keeps it.
  await editor.getByLabel("Label").fill("work laptop");
  await editor.getByRole("button", { name: "Save", exact: true }).click();
  await expect(editor).toBeHidden();
  await expect(page.getByRole("list", { name: "Keys" })).toContainText("work laptop");
});
