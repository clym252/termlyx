// Reuses the Playwright installed in apps/tauri (website has no Playwright dependency).
import pw from "../../apps/tauri/node_modules/@playwright/test/index.js";
const { test, expect } = pw;

const RELEASES = "https://github.com/clym252/termlyx/releases/latest";
const WIN_UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Safari/537.36";
const IPHONE_UA = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";

test.describe("no-js downloads", () => {
  test.use({ javaScriptEnabled: false });
  test("both links visible with release href", async ({ page }) => {
    await page.goto("/");
    for (const id of ["#dl-mac", "#dl-win"]) {
      await expect(page.locator(id)).toBeVisible();
      await expect(page.locator(id)).toHaveAttribute("href", RELEASES);
    }
  });
});

test.describe("windows order", () => {
  test.use({ userAgent: WIN_UA });
  test("windows link first in hero", async ({ page }) => {
    // Chromium keeps the real userAgentData.platform; drop it so the UA string decides.
    await page.addInitScript(() => Object.defineProperty(navigator, "userAgentData", { value: undefined }));
    await page.goto("/");
    await expect(page.locator("[data-downloads]").first().locator("a").first()).toHaveAttribute("id", "dl-win");
  });
});

test.describe("iphone", () => {
  test.use({ userAgent: IPHONE_UA });
  test("desktop buttons and coming-soon note visible", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator("#dl-mac")).toBeVisible();
    await expect(page.locator("#dl-win")).toBeVisible();
    await expect(page.getByText("iOS & Android coming soon")).toBeVisible();
  });
});

test.describe("reduced motion", () => {
  test.use({ reducedMotion: "reduce" });
  test("html.reduced and no autoplay", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator("html")).toHaveClass(/\breduced\b/);
    const videos = await page.locator("video").count();
    test.skip(videos === 0, "no videos yet");
    expect(await page.locator("video[autoplay]").count()).toBe(0);
    expect(await page.locator("video:not([controls])").count()).toBe(0);
  });
});

test.describe("mobile width", () => {
  test.use({ viewport: { width: 375, height: 812 } });
  test("no horizontal overflow", async ({ page }) => {
    await page.goto("/");
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(375);
  });
});

test("lazy media: only the hero video loads on load", async ({ page }) => {
  const media = [];
  // Posters (.jpg) load eagerly by design; only clip files count.
  page.on("request", (r) => { if (/\/media\/[^?#]*\.(mp4|webm)(?:[?#]|$)/.test(r.url())) media.push(r.url()); });
  await page.goto("/", { waitUntil: "load" });
  test.skip((await page.locator("video").count()) === 0, "no videos yet");
  const hero = await page.locator("video[data-hero] source, video[data-hero]").evaluateAll(
    (els) => els.map((e) => e.currentSrc || e.src).filter(Boolean));
  const heroPaths = hero.map((u) => new URL(u).pathname);
  for (const u of media) expect(heroPaths).toContain(new URL(u).pathname);
});

test("hero clip pauses offscreen and plays when back", async ({ page }) => {
  await page.goto("/");
  const hero = page.locator("video[data-hero]");
  const paused = () => hero.evaluate((v) => v.paused);
  await expect.poll(paused).toBe(false);
  await page.locator("footer").scrollIntoViewIfNeeded();
  await expect.poll(paused).toBe(true);
  await hero.scrollIntoViewIfNeeded();
  await expect.poll(paused).toBe(false);
});

for (const name of ["walkthrough", "sync", "themes"]) {
  test(`${name} clip (preload none, no autoplay) plays once scrolled into view`, async ({ page }) => {
    await page.goto("/");
    const clip = page.locator(`video[data-clip="${name}"]`);
    expect(await clip.evaluate((v) => v.paused)).toBe(true);
    await clip.scrollIntoViewIfNeeded();
    await expect.poll(() => clip.evaluate((v) => !v.paused && v.currentTime > 0)).toBe(true);
  });
}

test("no third-party requests", async ({ page, baseURL }) => {
  const origin = new URL(baseURL).origin;
  const foreign = [];
  page.on("request", (r) => { if (/^https?:/.test(r.url()) && new URL(r.url()).origin !== origin) foreign.push(r.url()); });
  await page.goto("/", { waitUntil: "networkidle" });
  expect(foreign).toEqual([]);
});
