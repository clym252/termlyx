import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";
import { activePane, connectHost, createVault, nav, seedHost, sessionIds } from "./helpers";

async function output(page: Page, id: string, text: string) {
  await page.evaluate(
    ({ id, text }) => window.__tetherMock.control.output(id, new TextEncoder().encode(text)),
    { id, text },
  );
}

test("ip_addresses_in_output_are_highlighted_and_text_is_unchanged", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  const [id] = await sessionIds(page);
  await output(page, id, "Linux 6.8.0-138-generic\r\nLast login: Wed Oct 28 09:03:11 2026 from 192.168.1.174\r\n");

  const marks = activePane(page).locator(".term-highlight");
  await expect(marks).toHaveCount(1);
  // The terminal's own text is untouched: the IP is plain characters in its rows.
  await expect(activePane(page).locator(".xterm-rows")).toContainText("from 192.168.1.174");

  // Switched off in Settings → Terminal, the highlight goes.
  await nav(page).getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("checkbox", { name: "Highlight IP addresses" }).uncheck();
  await nav(page).getByRole("button", { name: /^Terminals/ }).click();
  await expect(marks).toHaveCount(0);
});

test("highlights_follow_the_screen_when_it_is_cleared_or_switched", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  const [id] = await sessionIds(page);
  const marks = activePane(page).locator(".term-highlight");

  await output(page, id, "from 192.168.1.174\r\n");
  await expect(marks).toHaveCount(1);
  // `clear`: the rows are blanked in place; new text there is not highlighted.
  await output(page, id, "\x1b[H\x1b[2Jtotal 48 drwxr-xr-x 2 root root 4096 Oct 28\r\n");
  await expect(marks).toHaveCount(0);

  // A full-screen program (alternate screen): nothing of the shell's highlights is left over it.
  await output(page, id, "login from 10.0.0.9\r\n");
  await expect(marks).toHaveCount(1);
  await output(page, id, "\x1b[?1049h\x1b[Hplain text in vim\r\n");
  await expect(marks).toHaveCount(0);
  // Back at the shell, its address is highlighted again (once).
  await output(page, id, "\x1b[?1049l");
  await expect(marks).toHaveCount(1);
});
