import { expect, test } from "./fixtures";
import { PASSWORD, activePane, createVault, goTo, sessionIds, terminalTabs, workspaceKeys, written } from "./helpers";

// The page reports a Windows `navigator.platform` before any app code runs.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(Navigator.prototype, "platform", { get: () => "Win32", configurable: true });
  });
});

test("windows_labels_say_ctrl_and_never_cmd", async ({ page }) => {
  await createVault(page);
  await expect(page.locator(".search-button kbd")).toHaveText("Ctrl+Shift+K");

  for (const section of ["Hosts", "Keychain", "Port Forwarding", "Known Hosts", "SFTP", "Settings"]) {
    await goTo(page, section);
    const text = await page.locator("body").innerText();
    expect(text, section).not.toContain("⌘");
    expect(text, section).not.toContain("⇧");
    expect(text, section).not.toMatch(/\bCmd\b|\bMac\b|Touch ID|Finder/);
  }

  // Quick unlock names the Windows store, not the macOS keychain.
  await goTo(page, "Settings");
  const security = page.getByRole("form", { name: "Security" });
  await expect(security.getByText("Unlock with Credential Manager")).toBeVisible();
  await expect(security).not.toContainText(/keychain/i);
});

test("windows_palette_key_leaves_ctrl_k_to_the_terminal", async ({ page }) => {
  await createVault(page);
  const dialog = page.getByRole("dialog", { name: "Search" });

  // Plain Ctrl+K opens the palette outside a terminal.
  await page.keyboard.press("Control+k");
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();

  await page.keyboard.press(workspaceKeys(false).localTerminal);
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect(activePane(page)).toHaveCount(1);
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  const [id] = await sessionIds(page);

  // In the terminal, Ctrl+K and Ctrl+C (nothing selected) are the shell's.
  await page.keyboard.press("Control+k");
  await page.keyboard.press("Control+c");
  await expect.poll(() => written(page, id)).toBe("\x0b\x03");
  await expect(dialog).toBeHidden();

  // Ctrl+Shift+K opens the palette with the terminal focused, and sends nothing.
  await page.keyboard.press("Control+Shift+k");
  await expect(dialog).toBeVisible();
  expect(await written(page, id)).toBe("\x0b\x03");
});

test("windows_hello_quick_unlock_screens", async ({ page }) => {
  // A locked vault with Windows Hello unlock on, and Hello not usable at launch (no PIN set up).
  await page.addInitScript((password) => {
    window.__tetherMockSeed = {
      quickUnlockKind: "windowsHello",
      lockedVault: { password, quickUnlock: true },
      quickUnlockFailure: {
        message: "Set up a Windows Hello PIN in Windows Settings to use quick unlock.",
        code: "biometryUnavailable",
      },
    };
  }, PASSWORD);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
  // The automatic attempt asks once and stays quiet; the password field is ready.
  await expect.poll(() => page.evaluate(() => window.__tetherMock.control.quickUnlockCalls())).toBe(1);
  await expect(page.getByRole("alert")).toHaveCount(0);
  await expect(page.getByLabel("Master password")).toBeFocused();
  await expect(page.locator("body")).not.toContainText(/Touch ID|keychain/i);

  // Asking by hand says why.
  await page.evaluate(() =>
    window.__tetherMock.control.failQuickUnlock(
      "unlock",
      "Set up a Windows Hello PIN in Windows Settings to use quick unlock.",
      "biometryUnavailable",
    ),
  );
  await page.getByRole("button", { name: "Unlock with Windows Hello" }).click();
  await expect(page.getByRole("alert")).toContainText("Set up a Windows Hello PIN in Windows Settings");

  // The password works, and Settings names Hello.
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await goTo(page, "Settings");
  const security = page.getByRole("form", { name: "Security" });
  await expect(security.getByRole("checkbox", { name: "Unlock with Windows Hello" })).toBeChecked();
  await expect(security).toContainText("only Windows Hello (your PIN, fingerprint or face) can open");
  await expect(security).not.toContainText(/Touch ID|keychain|Credential Manager/i);

  // A reset Hello credential: the Windows wording, and the button goes away.
  await page.getByRole("button", { name: "Lock vault" }).click();
  await page.evaluate(() =>
    window.__tetherMock.control.failQuickUnlock(
      "unlock",
      "Windows Hello changed; quick unlock was turned off",
      "biometryChanged",
    ),
  );
  await page.getByRole("button", { name: "Unlock with Windows Hello" }).click();
  await expect(page.getByRole("alert")).toHaveText(
    "Windows Hello changed, so quick unlock was turned off. Unlock with your master password, then turn it on again in Settings → Security.",
  );
  await expect(page.getByRole("button", { name: "Unlock with Windows Hello" })).toHaveCount(0);
});
