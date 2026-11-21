import { expect, test } from "./fixtures";
import type { Page } from "@playwright/test";
import { IPAD_AIR_LANDSCAPE, IPHONE_15_PRO, PASSWORD, createVault, emulateIos, goTo, seedHost } from "./helpers";

// Words that name another platform. "Keychain" stays: it is Termlyx's own page.
const OTHER_PLATFORM = /\bMacs?\b|Finder|\bCmd\b|Windows|\bPCs?\b|\bcomputers?\b|Keychain Access|computer's keychain/i;

/** Everything a person can read or hear on the page: its text and its placeholder, aria-label, title and alt attributes. */
async function words(page: Page): Promise<string> {
  return page.evaluate(() => {
    const attrs = [...document.querySelectorAll("[placeholder],[aria-label],[title],[alt]")].flatMap((el) =>
      ["placeholder", "aria-label", "title", "alt"].map((a) => el.getAttribute(a) ?? ""),
    );
    return [document.body.innerText, ...attrs].join("\n");
  });
}

const SECTIONS = ["Hosts", "Keychain", "Port Forwarding", "Known Hosts", "SFTP", "Settings"];

for (const [device, size] of [
  ["iPhone", IPHONE_15_PRO],
  ["iPad", IPAD_AIR_LANDSCAPE],
] as const) {
  test.describe(device, () => {
    test.beforeEach(async ({ page }) => {
      await emulateIos(page, size);
    });

    test(`${device}_pages_never_name_another_platform`, async ({ page }) => {
      await createVault(page);
      await seedHost(page, "web-1", "web1.example");
      for (const section of SECTIONS) {
        await goTo(page, section);
        await expect(page.getByRole("heading", { name: section, level: 1 })).toBeVisible();
        expect(await words(page), section).not.toMatch(OTHER_PLATFORM);
      }
      // The forms and dialogs the pages open.
      await goTo(page, "Hosts");
      await page.getByRole("button", { name: "New host", exact: true }).click();
      const form = page.getByRole("form", { name: "Host details" });
      await expect(form).toBeVisible();
      expect(await words(page), "host form").not.toMatch(OTHER_PLATFORM);
      await goTo(page, "Settings");
      await expect(page.getByRole("form", { name: "Security" })).toBeVisible();
      expect(await page.locator("body").innerText(), "settings").not.toContain("keychain");
    });

    test(`${device}_offers_no_ssh_file_imports`, async ({ page }) => {
      await createVault(page);
      await goTo(page, "Hosts");
      await expect(page.getByRole("button", { name: "New host", exact: true })).toBeVisible();
      await expect(page.getByRole("button", { name: "Import SSH config…" })).toHaveCount(0);
      await goTo(page, "Known Hosts");
      await expect(page.getByRole("heading", { name: "Known Hosts", level: 1 })).toBeVisible();
      await expect(page.getByRole("button", { name: /^Import/ })).toHaveCount(0);
      await expect(page.getByText("No known hosts yet.", { exact: false })).not.toContainText("import");
    });

    test(`${device}_dialogs_never_name_another_platform`, async ({ page }) => {
      await createVault(page);
      await seedHost(page, "web-1", "web1.example");

      // Port Forwarding: the rule editor, with each kind's help.
      await goTo(page, "Port Forwarding");
      await page.getByRole("button", { name: "New rule" }).click();
      const rule = page.getByRole("dialog", { name: "New rule" });
      await expect(rule).toBeVisible();
      for (const kind of ["Local", "Remote", "Dynamic (SOCKS)"]) {
        await rule.getByLabel("Type").selectOption({ label: kind });
        expect(await words(page), `rule ${kind}`).not.toMatch(OTHER_PLATFORM);
      }
      await expect(rule).toContainText(`this ${device}`);
      await page.keyboard.press("Escape");

      // Settings → Sync → Connect to server.
      await goTo(page, "Settings");
      await page.getByRole("button", { name: "Connect to server" }).click();
      const enable = page.getByRole("dialog", { name: "Connect to server" });
      await expect(enable).toBeVisible();
      expect(await words(page), "turn on sync").not.toMatch(OTHER_PLATFORM);
    });

    test(`${device}_sync_add_device_never_names_another_platform`, async ({ page }) => {
      await page.addInitScript((password) => {
        window.__tetherMockSeed = { lockedVault: { password, quickUnlock: false }, sync: {} };
      }, PASSWORD);
      await page.goto("/");
      await page.getByLabel("Master password").fill(PASSWORD);
      await page.getByRole("button", { name: "Unlock", exact: true }).click();
      await goTo(page, "Settings");
      expect(await words(page), "settings with sync on").not.toMatch(OTHER_PLATFORM);
      await page.getByRole("button", { name: "Add device" }).click();
      await expect(page.getByRole("dialog", { name: "Add a device" })).toBeVisible();
      expect(await words(page), "add device").not.toMatch(OTHER_PLATFORM);
    });

    test(`${device}_join_a_synced_vault_never_names_another_platform`, async ({ page }) => {
      await page.goto("/");
      await expect(page.getByRole("heading", { name: "Create your vault" })).toBeVisible();
      expect(await words(page), "create vault").not.toMatch(OTHER_PLATFORM);
      await page.getByRole("button", { name: "Join a synced vault" }).click();
      await expect(page.getByRole("heading", { name: "Join a synced vault" })).toBeVisible();
      expect(await words(page), "join").not.toMatch(OTHER_PLATFORM);
    });

    test(`${device}_tells_the_backend_what_to_call_it_in_the_sync_device_list`, async ({ page }) => {
      await createVault(page);
      await expect.poll(() => page.evaluate(() => window.__tetherMock.control.deviceFallback())).toBe(device);
    });

    test(`${device}_quick_unlock_says_face_id_or_touch_id_when_the_sensor_is_unknown`, async ({ page }) => {
      // The backend could not tell which sensor the device has.
      await page.addInitScript((password) => {
        window.__tetherMockSeed = { quickUnlockKind: "none", lockedVault: { password, quickUnlock: true } };
      }, PASSWORD);
      await page.goto("/");
      await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
      await expect(page.getByRole("button", { name: "Unlock with Face ID or Touch ID" })).toBeVisible();
      await expect(page.getByRole("button", { name: "Quick unlock" })).toHaveCount(0);
      await page.getByLabel("Master password").fill(PASSWORD);
      await page.getByRole("button", { name: "Unlock", exact: true }).click();
      await goTo(page, "Settings");
      await expect(page.getByRole("checkbox", { name: "Unlock with Face ID or Touch ID" })).toBeVisible();
    });
  });
}

test("the_username_field_does_not_ask_for_a_local_login_on_ios", async ({ page }) => {
  await emulateIos(page);
  await createVault(page);
  await page.getByRole("button", { name: "New host", exact: true }).click();
  const form = page.getByRole("form", { name: "Host details" });
  await form.getByLabel("Identity", { exact: true }).selectOption({ label: "Own username & password" });
  await expect(form.getByLabel("Username")).toHaveAttribute("placeholder", "Login name on the server");
});

// A touch screen: `hover: none`, `pointer: coarse`.
test.describe("touch-only controls", () => {
  test.use({ hasTouch: true, isMobile: true });

  async function ipadWithFolderAndHost(page: Page) {
    await emulateIos(page, IPAD_AIR_LANDSCAPE);
    await createVault(page);
    await seedHost(page, "web-1", "web1.example");
  }

  test("ipad_host_cards_show_edit_without_hover_and_the_card_connects", async ({ page }) => {
    await ipadWithFolderAndHost(page);
    expect(await page.evaluate(() => matchMedia("(hover: none)").matches)).toBe(true);
    const edit = page.getByRole("button", { name: "Edit host web-1" });
    await expect(edit).toBeVisible();
    await expect(edit).toHaveCSS("opacity", "1");
    await expect(edit).toHaveCSS("pointer-events", "auto");
    // Connect is the card itself; the text button would only repeat it.
    await expect(page.getByRole("button", { name: "Connect to web-1" })).toBeHidden();
    const box = (await edit.boundingBox())!;
    expect(box.width).toBeGreaterThanOrEqual(44);
    expect(box.height).toBeGreaterThanOrEqual(44);
  });

  test("buttons_and_fields_reach_44_points", async ({ page }) => {
    await ipadWithFolderAndHost(page);
    await page.getByRole("button", { name: "New host", exact: true }).click();
    const form = page.getByRole("form", { name: "Host details" });
    for (const control of [
      page.getByRole("searchbox", { name: "Filter hosts" }),
      page.getByRole("button", { name: "New host", exact: true }),
      page.getByRole("button", { name: "New group", exact: true }),
      form.getByLabel("Label"),
      form.getByRole("button", { name: "Add tag" }),
    ]) {
      expect((await control.boundingBox())!.height).toBeGreaterThanOrEqual(44);
    }
  });
});

test("a_desktop_never_sets_a_device_fallback", async ({ page }) => {
  await createVault(page);
  expect(await page.evaluate(() => window.__tetherMock.control.deviceFallback())).toBeNull();
});
