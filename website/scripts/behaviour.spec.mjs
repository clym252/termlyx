import { readFileSync } from "node:fs";
// Reuses the Playwright installed in apps/tauri (website has no Playwright dependency).
import pw from "../../apps/tauri/node_modules/@playwright/test/index.js";
const { test, expect } = pw;

const DMG = "https://github.com/clym252/termlyx/releases/latest/download/Termlyx_universal.dmg";
const EXE = "https://github.com/clym252/termlyx/releases/latest/download/Termlyx_x64-setup.exe";
const WIN_UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Safari/537.36";
const MAC_UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Safari/537.36";
const LINUX_UA = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Safari/537.36";
const IPHONE_UA = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
// Chromium keeps the real userAgentData.platform; drop it so the UA string decides.
const uaOnly = (page) => page.addInitScript(() => Object.defineProperty(navigator, "userAgentData", { value: undefined }));

test.describe("no-js downloads", () => {
  test.use({ javaScriptEnabled: false });
  test("hero and nav jump to the download section, which links both installers", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator("#dl-primary")).toHaveAttribute("href", "#download");
    await expect(page.locator("#nav-download")).toHaveAttribute("href", "#download");
    await expect(page.locator("#dl-mac")).toHaveAttribute("href", DMG);
    await expect(page.locator("#dl-win")).toHaveAttribute("href", EXE);
    await expect(page.locator("#dl-mac")).toBeVisible();
    await expect(page.locator("#dl-win")).toBeVisible();
  });
});

for (const [name, ua, href, label] of [["mac", MAC_UA, DMG, "Download for macOS"], ["windows", WIN_UA, EXE, "Download for Windows"]]) {
  test.describe(`${name} visitor`, () => {
    test.use({ userAgent: ua });
    test("nav and hero download the installer directly", async ({ page }) => {
      await uaOnly(page);
      await page.goto("/");
      await expect(page.locator("#dl-primary")).toHaveAttribute("href", href);
      await expect(page.locator("#dl-primary")).toHaveText(label);
      await expect(page.locator("#nav-download")).toHaveAttribute("href", href);
    });
  });
}

test.describe("windows order", () => {
  test.use({ userAgent: WIN_UA });
  test("windows link first in the download section", async ({ page }) => {
    await uaOnly(page);
    await page.goto("/");
    await expect(page.locator("[data-downloads] a").first()).toHaveAttribute("id", "dl-win");
  });
});

for (const [name, ua] of [["iphone", IPHONE_UA], ["linux", LINUX_UA]]) {
  test.describe(name, () => {
    test.use({ userAgent: ua });
    test("buttons jump to the download section", async ({ page }) => {
      await uaOnly(page);
      await page.goto("/");
      await expect(page.locator("#dl-primary")).toHaveAttribute("href", "#download");
      await expect(page.locator("#dl-primary")).toHaveText("Download the beta");
      await expect(page.locator("#nav-download")).toHaveAttribute("href", "#download");
      if (name === "iphone") await expect(page.getByText("iOS & Android coming soon")).toBeVisible();
    });
  });
}

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

test("clicking the hero pauses it for good; Enter and Space toggle", async ({ page }) => {
  await page.goto("/");
  const hero = page.locator("video[data-hero]");
  const paused = () => hero.evaluate((v) => v.paused);
  await expect.poll(paused).toBe(false);
  await hero.click();
  expect(await paused()).toBe(true);
  await page.locator("footer").scrollIntoViewIfNeeded();
  await hero.scrollIntoViewIfNeeded();
  await page.waitForTimeout(500);
  expect(await paused()).toBe(true);
  await hero.focus();
  await page.keyboard.press("Enter");
  expect(await paused()).toBe(false);
  await page.keyboard.press("Space");
  expect(await paused()).toBe(true);
});

test("self-host box: tabs switch the command, copy reports Copied", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto("/");
  await page.getByRole("button", { name: "Windows", exact: true }).click();
  await expect(page.locator("[data-cmd]")).toContainText("install.ps1 | iex");
  await page.getByRole("button", { name: "Copy" }).click();
  await expect(page.getByRole("button", { name: "Copied" })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toContain("install.ps1 | iex");
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

test.describe("csp", () => {
  // Serve the pages with the exact CSP from src/_headers and fail on any violation.
  const csp = readFileSync(new URL("../src/_headers", import.meta.url), "utf8").match(/Content-Security-Policy: (.*)/)[1];
  for (const path of ["/", "/privacy.html", "/404.html"]) {
    test(`no violations on ${path}`, async ({ page }) => {
      await page.route("**/*", async (route) => {
        const res = await route.fetch();
        await route.fulfill({ response: res, headers: { ...res.headers(), "content-security-policy": csp } });
      });
      const problems = [];
      page.on("console", (m) => /content security policy|refused to/i.test(m.text()) && problems.push(m.text()));
      await page.addInitScript(() =>
        document.addEventListener("securitypolicyviolation", (e) => console.error("Refused to: " + e.violatedDirective + " " + e.blockedURI)));
      await page.goto(path);
      await page.mouse.wheel(0, 20000);
      await page.waitForTimeout(1500);
      expect(problems).toEqual([]);
    });
  }
});
