// Reference shots of the app as it is today, on its browser mock (not shipped).
// Shoot a production build: the dev server adds DEV-only items (the "Dev terminal" nav entry)
// that release builds never show. Build outside the repo and serve it:
//   VITE_TETHER_API=mock pnpm -C apps/tauri exec vite build --outDir "$TMPDIR/termlyx-mock" --emptyOutDir
//   pnpm -C apps/tauri exec vite preview --outDir "$TMPDIR/termlyx-mock" --host 127.0.0.1 --port 14299
// Then:  node website/clips/reference/shoot.mjs   (writes PNGs next to this file)
import { fileURLToPath } from "node:url";
import pw from "../../../apps/tauri/node_modules/@playwright/test/index.js";

const OUT = fileURLToPath(new URL(".", import.meta.url));
const URL_ = process.env.APP_URL ?? "http://127.0.0.1:14299/";
const PASSWORD = "correct horse battery"; // the e2e suite's mock vault password (apps/tauri/e2e/helpers.ts)

const browser = await pw.chromium.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 810 }, deviceScaleFactor: 2 });
await page.addInitScript(() => Object.defineProperty(Navigator.prototype, "platform", { get: () => "MacIntel", configurable: true }));
const shot = async (name) => { await page.waitForTimeout(500); await page.screenshot({ path: `${OUT}${name}.png` }); };
const enc = (s) => Array.from(new TextEncoder().encode(s.replace(/\n/g, "\r\n")));
const out = (i, text) => page.evaluate(async ({ i, b }) => {
  const s = (await window.__tetherMock.activeWork()).sessions;
  window.__tetherMock.control.output(s[i].id, new Uint8Array(b));
}, { i, b: enc(text) });
const G = "\x1b[32m", B = "\x1b[34m", Y = "\x1b[33m", R = "\x1b[0m", BO = "\x1b[1m";

// Vault and the clip hosts.
await page.goto(URL_);
await page.getByLabel("Master password", { exact: true }).fill(PASSWORD);
await page.getByLabel("Confirm master password").fill(PASSWORD);
await page.getByRole("button", { name: "Create vault" }).click();
await page.getByRole("dialog", { name: "Your recovery key" }).getByRole("button", { name: "I saved it" }).click();
await page.getByRole("heading", { name: "Hosts", level: 1 }).waitFor();
const ids = await page.evaluate(async () => {
  const api = window.__tetherMock;
  // The same records in the same order as the other clips' shots, so ids and badge colours
  // match (build-box, nas, pi-hole, db-01); a snippet before web-01 gives it a colour of its own.
  const admin = await api.upsertEntity("identity", { label: "admin", username: "admin", password: "pw" });
  const deploy = await api.upsertEntity("identity", { label: "deploy", username: "deploy", password: "pw" });
  const hosts = [["build-box", "192.168.1.40", deploy], ["nas", "192.168.1.30", admin], ["pi-hole", "192.168.1.2", admin], ["db-01", "192.168.1.20", admin]];
  const ids = {};
  for (const [label, address, who] of hosts) ids[label] = (await api.upsertEntity("host", { label, address, settings: { identityId: who.id } })).id;
  await api.upsertEntity("snippet", { label: "disk usage", text: "df -h", run: true });
  ids["web-01"] = (await api.upsertEntity("host", { label: "web-01", address: "192.168.1.10", settings: { identityId: deploy.id } })).id;
  return ids;
});
const nav = page.getByRole("navigation", { name: "Main" });
const go = (name) => nav.getByRole("button", { name, exact: true }).click();
await go("Settings"); await go("Hosts"); // the Hosts page reloads when shown again
await shot("01-hosts");

// Connect web-01 (held at its first step for the connecting card).
const list = page.getByRole("list", { name: "Hosts" });
await list.getByRole("listitem").filter({ has: page.getByRole("button", { name: "web-01", exact: true }) }).hover();
await shot("01b-hosts-hover");
await page.evaluate(() => { window.__hold = window.__tetherMock.control.holdNextOpenSession(); });
await list.getByRole("button", { name: "Connect to web-01", exact: true }).click();
await shot("02a-connecting");
await page.evaluate(() => window.__hold.release());
await page.waitForTimeout(600);
await out(0, `${BO}${G}deploy@web-01${R}:${BO}${B}~${R}$ systemctl status nginx\n${G}●${R} nginx.service - A high performance web server\n     Loaded: loaded (/lib/systemd/system/nginx.service; enabled)\n     Active: ${BO}${G}active (running)${R} since Mon 09:12:04 UTC; 1 day ago\n   Main PID: 812 (nginx)\n      Tasks: 5 (limit: 4557)\n     Memory: 11.8M\n${BO}${G}deploy@web-01${R}:${BO}${B}~${R}$ `);
await shot("02-terminal");

// Split right with db-01.
await page.getByRole("button", { name: "Layout" }).click();
await shot("03-layout-menu");
await page.getByRole("menuitem", { name: "Split right…", exact: true }).click();
await shot("04-split-launcher");
await page.keyboard.type("db-01");
await shot("04b-palette-typed");
await page.keyboard.press("Enter");
await page.waitForTimeout(800);
await out(1, `${BO}${G}admin@db-01${R}:${BO}${B}~${R}$ git log --oneline\n${Y}a41c9e2${R} migrate: add index on events\n${Y}7be01d4${R} backup: nightly to nas\n${Y}19f3c08${R} tune: shared_buffers 2GB\n${Y}e02d5b1${R} fix: vacuum schedule\n${BO}${G}admin@db-01${R}:${BO}${B}~${R}$ `);
await shot("05-split");

// Forwarding rules on web-01, one started.
const rule = await page.evaluate(async (hostId) => {
  const api = window.__tetherMock;
  const r = await api.upsertEntity("forward_rule", { hostId, label: "postgres", kind: "local", bindAddr: "127.0.0.1", bindPort: 5432, destHost: "db-01", destPort: 5432, autostart: false });
  await api.upsertEntity("forward_rule", { hostId, label: "socks", kind: "dynamic", bindAddr: "127.0.0.1", bindPort: 1080, autostart: false });
  return r.id;
}, ids["web-01"]);
await go("Port Forwarding");
await shot("06-forwarding-stopped");
await page.evaluate((id) => window.__tetherMock.forwardStart(id), rule);
await shot("07-forwarding-running");

// SFTP on web-01; Settings (Sync section); a theme change from the side panel.
await go("SFTP");
await page.getByLabel("Host", { exact: true }).selectOption({ label: "web-01" });
await page.waitForTimeout(800);
await shot("08-sftp");
await go("Settings");
await shot("09-settings-sync");
await nav.getByRole("button", { name: /Terminals/ }).click();
await page.getByRole("button", { name: "Side panel" }).click();
await page.getByRole("group", { name: "Show" }).getByRole("button", { name: "Themes" }).click();
await shot("10-themes");
await page.getByText("Dracula", { exact: true }).click();
await shot("11-theme-dracula");
await browser.close();
