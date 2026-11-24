// Reference shots for the sync clip: db-01's port edited on a Mac, the change arriving on a
// Windows PC, on the app's browser mock (not shipped) with sync on. Serve a production mock build
// first, as shoot.mjs says, then:
//   node website/clips/reference/shoot-sync.mjs   (writes PNGs next to this file)
// DUMP=<dir> also writes each state's DOM there, to rebuild the clip from the app's own markup.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import pw from "../../../apps/tauri/node_modules/@playwright/test/index.js";

const OUT = fileURLToPath(new URL(".", import.meta.url));
const URL_ = process.env.APP_URL ?? "http://127.0.0.1:14299/";
const PASSWORD = "correct horse battery"; // the e2e suite's mock vault password (apps/tauri/e2e/helpers.ts)

const browser = await pw.chromium.launch();

// One device: sync already on (the mock's seed), the vault and the same four hosts.
async function device(os, platform) {
  const page = await browser.newPage({ viewport: { width: 1440, height: 810 }, deviceScaleFactor: 2 });
  await page.addInitScript(({ os, platform }) => {
    window.__tetherMockSeed = { sync: {} };
    window.__TETHER_OS__ = os;
    Object.defineProperty(Navigator.prototype, "platform", { get: () => platform, configurable: true });
  }, { os, platform });
  await page.goto(URL_);
  await page.getByLabel("Master password", { exact: true }).fill(PASSWORD);
  await page.getByLabel("Confirm master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Create vault" }).click();
  await page.getByRole("dialog", { name: "Your recovery key" }).getByRole("button", { name: "I saved it" }).click();
  await page.getByRole("heading", { name: "Hosts", level: 1 }).waitFor();
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    const admin = await api.upsertEntity("identity", { label: "admin", username: "admin", password: "pw" });
    const deploy = await api.upsertEntity("identity", { label: "deploy", username: "deploy", password: "pw" });
    // db-01 last, as the walkthrough added it: the same ids, so the same badge colours.
    const hosts = [["build-box", "192.168.1.40", deploy], ["nas", "192.168.1.30", admin], ["pi-hole", "192.168.1.2", admin], ["db-01", "192.168.1.20", admin]];
    for (const [label, address, who] of hosts) {
      const h = await api.upsertEntity("host", { label, address, settings: { identityId: who.id } });
      if (label === "db-01") window.__db = { id: h.id, identityId: admin.id };
    }
  });
  const nav = page.getByRole("navigation", { name: "Main" });
  await nav.getByRole("button", { name: "Settings", exact: true }).click();
  await nav.getByRole("button", { name: "Hosts", exact: true }).click(); // the Hosts page reloads when shown again
  return page;
}
const shooter = (page) => async (name) => {
  await page.waitForTimeout(500);
  await page.screenshot({ path: `${OUT}${name}.png` });
  if (process.env.DUMP) writeFileSync(`${process.env.DUMP}/${name}.html`, await page.locator(".shell").first().evaluate((e) => e.outerHTML));
};

// Mac: edit db-01, Port 22 -> 2222, Save host (the save nudges a push: Syncing…, then Synced).
{
  const page = await device("macos", "MacIntel");
  const shot = shooter(page);
  await shot("30-mac-hosts");
  await page.getByRole("button", { name: "db-01", exact: true }).hover();
  await shot("30b-mac-hover");
  await page.getByRole("button", { name: "Edit host db-01" }).click();
  await shot("31-mac-edit");
  const form = page.getByRole("form", { name: "Host details" });
  await form.getByRole("spinbutton", { name: "Port" }).fill("2222");
  await shot("32-mac-port");
  await form.getByRole("button", { name: "Save host" }).click();
  await page.evaluate(() => window.__tetherMock.control.setSyncStatus({ state: "syncing", done: 0, total: 1 }));
  await shot("33-mac-saved-syncing");
  await page.close();
}

// Windows PC: the same vault; sync pulls the change in (vault-changed), the list reads again.
{
  const page = await device("windows", "Win32");
  const shot = shooter(page);
  await shot("34-pc-hosts");
  await page.evaluate(() => window.__tetherMock.control.setSyncStatus({ state: "syncing", done: 0, total: 1 }));
  await page.evaluate(() => {
    const { id, identityId } = window.__db;
    window.__tetherMock.control.remoteUpsert("host", { id, label: "db-01", address: "192.168.1.20", settings: { identityId, port: 2222 } });
  });
  await shot("35-pc-syncing");
  await page.evaluate(() => window.__tetherMock.control.setSyncStatus({ state: "idle", lastSync: Date.now() }));
  await shot("36-pc-synced");
  await page.close();
}
await browser.close();
