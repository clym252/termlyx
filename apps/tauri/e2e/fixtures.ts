import { expect, test as base } from "@playwright/test";

/**
 * `test` that fails when the page logs a console error or warning (React
 * warnings included) or throws, so a green run is also a clean one.
 */
export const test = base.extend<{ cleanConsole: void; pinnedPlatform: void }>({
  // The specs are written for macOS wording and chords (Cmd, Touch ID, keychain); the host's
  // own Chromium would report Win32 or Linux. Pinned before any app code runs, so the suite
  // gives the same result on every host. Specs for another platform override it with their
  // own init script (emulateLinux, windows-labels.spec), which runs later and wins.
  pinnedPlatform: [
    async ({ page }, use) => {
      await page.addInitScript(() => {
        Object.defineProperty(Navigator.prototype, "platform", { get: () => "MacIntel", configurable: true });
      });
      await use();
    },
    { auto: true },
  ],
  cleanConsole: [
    async ({ page }, use) => {
      const problems: string[] = [];
      page.on("console", (m) => {
        if (m.type() === "error" || m.type() === "warning") problems.push(`${m.type()}: ${m.text()}`);
      });
      page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
      await use();
      expect(problems).toEqual([]);
    },
    { auto: true },
  ],
});

export { expect };
