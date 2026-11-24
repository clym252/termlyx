// Reference shots for the walkthrough clip: the desktop host editor and the phone layout, on the
// app's browser mock (not shipped). Serve a production mock build first, as shoot.mjs says, then:
//   node website/clips/reference/shoot-walkthrough.mjs   (writes PNGs next to this file)
// DUMP=<dir> also writes each state's DOM there, to rebuild the clip from the app's own markup.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import pw from "../../../apps/tauri/node_modules/@playwright/test/index.js";

const OUT = fileURLToPath(new URL(".", import.meta.url));
const URL_ = process.env.APP_URL ?? "http://127.0.0.1:14299/";
const PASSWORD = "correct horse battery"; // the e2e suite's mock vault password (apps/tauri/e2e/helpers.ts)
const IPHONE_15_PRO = { width: 393, height: 852 }; // apps/tauri/e2e/helpers.ts

const browser = await pw.chromium.launch();

async function vault(page) {
  await page.goto(URL_);
  await page.getByLabel("Master password", { exact: true }).fill(PASSWORD);
  await page.getByLabel("Confirm master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Create vault" }).click();
  await page.getByRole("dialog", { name: "Your recovery key" }).getByRole("button", { name: "I saved it" }).click();
  await page.getByRole("heading", { name: "Hosts", level: 1 }).waitFor();
}
// The hosts already in the vault (the same on both devices).
const seed = (page) => page.evaluate(async () => {
  const api = window.__tetherMock;
  const admin = await api.upsertEntity("identity", { label: "admin", username: "admin", password: "pw" });
  const deploy = await api.upsertEntity("identity", { label: "deploy", username: "deploy", password: "pw" });
  const hosts = [["build-box", "192.168.1.40", deploy], ["nas", "192.168.1.30", admin], ["pi-hole", "192.168.1.2", admin]];
  for (const [label, address, who] of hosts) await api.upsertEntity("host", { label, address, settings: { identityId: who.id } });
});
const shooter = (page) => async (name, root = "body") => {
  await page.waitForTimeout(500);
  await page.screenshot({ path: `${OUT}${name}.png` });
  if (process.env.DUMP) writeFileSync(`${process.env.DUMP}/${name}.html`, await page.locator(root).first().evaluate((e) => e.outerHTML));
};

// Desktop (Mac): New host -> db-01, 192.168.1.20, own login admin -> Save host.
{
  const page = await browser.newPage({ viewport: { width: 1440, height: 810 }, deviceScaleFactor: 2 });
  await page.addInitScript(() => Object.defineProperty(Navigator.prototype, "platform", { get: () => "MacIntel", configurable: true }));
  const shot = shooter(page);
  await vault(page);
  await seed(page);
  const nav = page.getByRole("navigation", { name: "Main" });
  await nav.getByRole("button", { name: "Settings", exact: true }).click();
  await nav.getByRole("button", { name: "Hosts", exact: true }).click(); // the Hosts page reloads when shown again
  await page.getByRole("button", { name: "New host", exact: true }).click();
  await shot("20-editor-new", ".main");
  const form = page.getByRole("form", { name: "Host details" });
  await form.getByLabel("Label", { exact: true }).fill("db-01");
  await form.getByLabel("Address", { exact: true }).fill("192.168.1.20");
  await form.getByLabel("Identity", { exact: true }).selectOption({ label: "Own username & password" });
  await form.getByLabel("Username", { exact: true }).fill("admin");
  await form.getByLabel("Username", { exact: true }).focus();
  await shot("21-editor-filled", ".main");
  await form.getByRole("button", { name: "Save host" }).click();
  await shot("22-hosts-saved", ".main");
  await page.close();
}

// Phone (iPhone 15 Pro, as e2e emulateIos): the host list before and after sync, then a tap on db-01.
{
  const page = await browser.newPage({ viewport: IPHONE_15_PRO, deviceScaleFactor: 3 });
  await page.addInitScript((s) => {
    window.__TETHER_OS__ = "ios";
    Object.defineProperty(Navigator.prototype, "platform", { get: () => "iPhone", configurable: true });
    Object.defineProperty(Screen.prototype, "width", { get: () => s.width, configurable: true });
    Object.defineProperty(Screen.prototype, "height", { get: () => s.height, configurable: true });
  }, IPHONE_15_PRO);
  const shot = shooter(page);
  await vault(page);
  await seed(page);
  const reload = async () => {
    await page.getByRole("button", { name: "Menu", exact: true }).click();
    const nav = page.getByRole("dialog", { name: "Menu" }).getByRole("navigation", { name: "Main" });
    await nav.getByRole("button", { name: "Settings", exact: true }).click();
    await page.getByRole("button", { name: "Menu", exact: true }).click();
    await page.getByRole("dialog", { name: "Menu" }).getByRole("navigation", { name: "Main" }).getByRole("button", { name: "Hosts", exact: true }).click();
  };
  await reload();
  await shot("23-phone-hosts");
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    const admin = await api.upsertEntity("identity", { label: "admin", username: "admin", password: "pw" });
    await api.upsertEntity("host", { label: "db-01", address: "192.168.1.20", settings: { identityId: admin.id } });
  });
  await reload();
  await shot("24-phone-hosts-db");
  await page.evaluate(() => { window.__hold = window.__tetherMock.control.holdNextOpenSession(); });
  await page.getByRole("list", { name: "Hosts" }).getByRole("button", { name: "db-01", exact: true }).click();
  await shot("25-phone-connecting");
  await page.evaluate(() => window.__hold.release());
  await page.waitForTimeout(600);
  // The mock prints nothing: the clip's prompt, as the hero's shots do it.
  await page.evaluate(async (b) => {
    const s = (await window.__tetherMock.activeWork()).sessions;
    window.__tetherMock.control.output(s[0].id, new Uint8Array(b));
  }, Array.from(new TextEncoder().encode("\x1b[1m\x1b[32madmin@db-01\x1b[0m:\x1b[1m\x1b[34m~\x1b[0m$ ")));
  await page.waitForTimeout(400);
  await shot("26-phone-terminal");
  await page.close();
}
await browser.close();
