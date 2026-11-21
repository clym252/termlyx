import { expect, test } from "./fixtures";
import type { CDPSession, Page } from "@playwright/test";
import { activePane, connectHost, createVault, emulateIos, seedHost, sessionIds, written } from "./helpers";

// A phone: real touch events through CDP, as on the device.
test.use({ hasTouch: true });

test.beforeEach(async ({ page }) => {
  await emulateIos(page);
});

/** A vault with one host, connected; returns the session id. */
async function connected(page: Page): Promise<string> {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  const [id] = await sessionIds(page);
  return id;
}

/** Session output, as the shell would send it. */
async function output(page: Page, id: string, text: string) {
  await page.evaluate(
    ({ id, text }) => window.__tetherMock.control.output(id, new TextEncoder().encode(text)),
    { id, text },
  );
}

const menu = (page: Page) => page.getByRole("menu", { name: "Terminal text" });
const handles = (page: Page) => activePane(page).locator(".touch-handle");
const clipboard = (page: Page) => page.evaluate(() => window.__tetherMock.control.clipboard());

/**
 * The centre of the `nth` character of `text` on the terminal (DOM renderer rows): where a
 * finger on it goes.
 */
async function charPoint(page: Page, text: string, nth = 0): Promise<{ x: number; y: number }> {
  const p = await activePane(page)
    .locator(".xterm-rows")
    .evaluate(
      (rows, { text, nth }) => {
        for (const row of Array.from(rows.children)) {
          const at = (row.textContent ?? "").indexOf(text);
          if (at < 0) continue;
          // Walk the row's text nodes to the character.
          let left = at + nth;
          const walk = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
          for (let n = walk.nextNode(); n; n = walk.nextNode()) {
            const len = n.textContent?.length ?? 0;
            if (left < len) {
              const r = document.createRange();
              r.setStart(n, left);
              r.setEnd(n, left + 1);
              const box = r.getBoundingClientRect();
              return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
            }
            left -= len;
          }
        }
        return null;
      },
      { text, nth },
    );
  expect(p, `"${text}" on screen`).not.toBeNull();
  return p!;
}

/** One finger held still at `at` for `ms`, then lifted. */
async function hold(cdp: CDPSession, at: { x: number; y: number }, ms = 700) {
  await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ ...at, id: 1 }] });
  await new Promise((r) => setTimeout(r, ms));
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
}

/** A quick tap. */
async function tap(cdp: CDPSession, at: { x: number; y: number }) {
  await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ ...at, id: 1 }] });
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
}

/** One finger from `from` to `to` in small steps, then lifted. */
async function drag(cdp: CDPSession, from: { x: number; y: number }, to: { x: number; y: number }, steps = 10) {
  await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ ...from, id: 1 }] });
  for (let i = 1; i <= steps; i++) {
    const p = { x: from.x + ((to.x - from.x) * i) / steps, y: from.y + ((to.y - from.y) * i) / steps };
    await new Promise((r) => setTimeout(r, 16));
    await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ ...p, id: 1 }] });
  }
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
}

test("ios_long_press_paste_sends_the_clipboard_as_typed_and_bracketed_when_asked", async ({ page }) => {
  const id = await connected(page);
  await output(page, id, "$ ");
  const cdp = await page.context().newCDPSession(page);
  await page.evaluate(() => window.__tetherMock.control.setClipboard("echo one\necho two"));

  await hold(cdp, await charPoint(page, "$"));
  await expect(menu(page).getByRole("menuitem")).toHaveText(["Paste", "Select", "Select All"]);
  // The press itself sent nothing.
  expect(await written(page, id)).toBe("");
  await menu(page).getByRole("menuitem", { name: "Paste" }).tap();
  await expect(menu(page)).toHaveCount(0);
  // As xterm pastes: line feeds become carriage returns.
  await expect.poll(() => written(page, id)).toBe("echo one\recho two");

  // The app turns on bracketed paste: the next paste is wrapped.
  await output(page, id, "\x1b[?2004h");
  await page.evaluate(() => window.__tetherMock.control.setClipboard("ls"));
  await hold(cdp, await charPoint(page, "$"));
  await menu(page).getByRole("menuitem", { name: "Paste" }).tap();
  await expect.poll(() => written(page, id)).toBe("echo one\recho two\x1b[200~ls\x1b[201~");
});

test("ios_select_picks_the_word_and_copy_puts_it_on_the_clipboard", async ({ page }) => {
  const id = await connected(page);
  await output(page, id, "cat /var/log/syslog | grep sshd\r\n$ ");
  const cdp = await page.context().newCDPSession(page);

  await hold(cdp, await charPoint(page, "syslog", 2));
  await menu(page).getByRole("menuitem", { name: "Select", exact: true }).tap();
  // The whole path (xterm's word separators), two handles, and the selection's menu.
  await expect(handles(page)).toHaveCount(2);
  await expect(menu(page).getByRole("menuitem")).toHaveText(["Copy", "Save as snippet", "Select All", "Done"]);
  await menu(page).getByRole("menuitem", { name: "Copy" }).tap();
  expect(await clipboard(page)).toBe("/var/log/syslog");
  // Copy ends the selection; nothing went to the session.
  await expect(menu(page)).toHaveCount(0);
  await expect(handles(page)).toHaveCount(0);
  expect(await written(page, id)).toBe("");
});

test("ios_save_as_snippet_opens_the_editor_with_the_selection", async ({ page }) => {
  const id = await connected(page);
  await output(page, id, "cat /var/log/syslog | grep sshd\r\n$ ");
  const cdp = await page.context().newCDPSession(page);
  await hold(cdp, await charPoint(page, "syslog", 2));
  await menu(page).getByRole("menuitem", { name: "Select", exact: true }).tap();
  await menu(page).getByRole("menuitem", { name: "Save as snippet" }).tap();
  const editor = page.getByRole("dialog", { name: "New snippet" });
  await expect(editor.getByLabel("Command")).toHaveValue("/var/log/syslog");
  await expect(menu(page)).toHaveCount(0);
  expect(await written(page, id)).toBe("");
});

test("ios_dragging_the_end_handle_extends_the_selection", async ({ page }) => {
  const id = await connected(page);
  await output(page, id, "alpha beta gamma delta\r\n");
  const cdp = await page.context().newCDPSession(page);

  await hold(cdp, await charPoint(page, "beta"));
  await menu(page).getByRole("menuitem", { name: "Select", exact: true }).tap();
  const end = handles(page).and(page.locator('[data-handle="end"]'));
  const box = (await end.boundingBox())!;
  // Grab the knob (below the row), drag it to the end of "gamma".
  const from = { x: box.x + box.width / 2, y: box.y + box.height - 12 };
  const target = await charPoint(page, "gamma", 4);
  const cell = target.x - (await charPoint(page, "gamma", 3)).x;
  await drag(cdp, from, { x: from.x + (target.x + cell / 2 - (box.x + box.width / 2)), y: from.y });
  // The menu comes back once the handle is let go.
  await expect(menu(page).getByRole("menuitem", { name: "Copy" })).toBeVisible();
  await menu(page).getByRole("menuitem", { name: "Copy" }).tap();
  expect(await clipboard(page)).toBe("beta gamma");
});

test("ios_select_all_copies_everything_and_a_tap_closes_the_menu", async ({ page }) => {
  const id = await connected(page);
  await output(page, id, "first line\r\nsecond line\r\n$ ");
  const cdp = await page.context().newCDPSession(page);

  // A tap closes the long press's menu without sending anything.
  await hold(cdp, await charPoint(page, "first"));
  await expect(menu(page)).toBeVisible();
  await tap(cdp, await charPoint(page, "second"));
  await expect(menu(page)).toHaveCount(0);

  await hold(cdp, await charPoint(page, "first"));
  await menu(page).getByRole("menuitem", { name: "Select All" }).tap();
  await menu(page).getByRole("menuitem", { name: "Copy" }).tap();
  expect(await clipboard(page)).toMatch(/^first line\nsecond line\n\$/);

  // Done drops a selection without copying.
  await page.evaluate(() => window.__tetherMock.control.setClipboard(null));
  await hold(cdp, await charPoint(page, "second"));
  await menu(page).getByRole("menuitem", { name: "Select", exact: true }).tap();
  await menu(page).getByRole("menuitem", { name: "Done" }).tap();
  await expect(handles(page)).toHaveCount(0);
  expect(await clipboard(page)).toBeNull();
  expect(await written(page, id)).toBe("");
});

test("ios_session_menu_paste_sends_the_clipboard", async ({ page }) => {
  const id = await connected(page);
  await page.evaluate(() => window.__tetherMock.control.setClipboard("uptime"));
  await page.getByRole("button", { name: "Session menu", exact: true }).click();
  await page.getByRole("menu", { name: "Session menu" }).getByRole("menuitem", { name: "Paste" }).click();
  await expect.poll(() => written(page, id)).toBe("uptime");
});
