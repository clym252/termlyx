// Renders og/card.html to src/og.png (1200x630). Reuses Playwright from apps/tauri.
import pw from "../../apps/tauri/node_modules/@playwright/test/index.js";
import { fileURLToPath } from "node:url";
const at = (p) => fileURLToPath(new URL(p, import.meta.url));
const browser = await pw.chromium.launch();
const page = await browser.newPage({ viewport: { width: 1200, height: 630 } });
await page.goto("file://" + at("card.html"));
await page.evaluate(() => document.fonts.ready);
await page.screenshot({ path: at("../src/og.png") });
await browser.close();
