import { expect, test } from "./fixtures";
import type { CDPSession, Page } from "@playwright/test";
import { activePane, connectHost, createVault, emulateIos, seedHost, sessionIds, written } from "./helpers";

// A phone: touch input (real touch events through CDP, so the browser's own scrolling acts on them).
test.use({ hasTouch: true });

test.beforeEach(async ({ page }, info) => {
  if (info.title.startsWith("ios_")) await emulateIos(page);
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

/** `seq 1 500`: 500 numbered lines, the terminal scrolled to the bottom. */
async function seq(page: Page, id: string) {
  const lines = Array.from({ length: 500 }, (_, i) => `${i + 1}`).join("\r\n");
  await output(page, id, `${lines}\r\n`);
  await expect.poll(() => firstRow(page)).toBeGreaterThan(400);
}

/** The number on the terminal's top visible row (DOM renderer rows), or NaN. */
async function firstRow(page: Page): Promise<number> {
  const text = await activePane(page).locator(".xterm-rows > div").first().textContent();
  return Number.parseInt((text ?? "").trim(), 10);
}

/** The drawn screen's sub-line shift (px, positive down; 0 at rest), from its transform. */
async function shift(page: Page): Promise<number> {
  const t = await activePane(page)
    .locator(".xterm-screen")
    .evaluate((el) => (el as HTMLElement).style.transform);
  if (!t) return 0;
  const m = /translate3d\(0(?:px)?, (-?[\d.e-]+)px, 0(?:px)?\)/.exec(t);
  expect(m, t).not.toBeNull();
  return Number(m![1]);
}

/** Where the view is, in px from line 1: the top row's line, less the shift (read after xterm has redrawn). */
async function viewPx(page: Page, rowPx: number): Promise<number> {
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  return ((await firstRow(page)) - 1) * rowPx - (await shift(page));
}

/** The terminal screen's box. */
async function screenBox(page: Page) {
  const box = await activePane(page).locator(".xterm-screen").boundingBox();
  expect(box).not.toBeNull();
  return box!;
}

/**
 * One finger from `from` to `to` in `steps` moves, `stepMs` apart, then lifted. Real touch
 * input (CDP), so default actions (native scrolling, the click after a tap) happen as on a phone.
 */
async function drag(
  cdp: CDPSession,
  from: { x: number; y: number },
  to: { x: number; y: number },
  { steps = 12, stepMs = 16 }: { steps?: number; stepMs?: number } = {},
) {
  const point = (p: { x: number; y: number }) => [{ x: p.x, y: p.y, id: 1 }];
  await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: point(from) });
  for (let i = 1; i <= steps; i++) {
    const p = { x: from.x + ((to.x - from.x) * i) / steps, y: from.y + ((to.y - from.y) * i) / steps };
    await new Promise((r) => setTimeout(r, stepMs));
    await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: point(p) });
  }
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
}

test("ios_a_vertical_drag_scrolls_the_scrollback", async ({ page }) => {
  const id = await connected(page);
  await seq(page, id);
  const bottom = await firstRow(page);
  const box = await screenBox(page);
  const cdp = await page.context().newCDPSession(page);

  // Finger down the screen: back into the scrollback, by about the distance dragged (no fling: slow moves).
  await drag(cdp, { x: box.x + box.width / 2, y: box.y + 40 }, { x: box.x + box.width / 2, y: box.y + 340 }, {
    steps: 30,
    stepMs: 30,
  });
  await expect.poll(() => firstRow(page)).toBeLessThan(bottom - 10);
  const up = await firstRow(page);
  expect(bottom - up).toBeLessThan(30);

  // Finger up the screen: forward again.
  await drag(cdp, { x: box.x + box.width / 2, y: box.y + 300 }, { x: box.x + box.width / 2, y: box.y + 100 }, {
    steps: 20,
    stepMs: 30,
  });
  await expect.poll(() => firstRow(page)).toBeGreaterThan(up + 5);
  // Scrolling sends nothing to the session.
  expect(await written(page, id)).toBe("");
});

test("ios_a_drag_moves_the_scrollback_by_the_pixel_and_rests_on_a_whole_line", async ({ page }) => {
  const id = await connected(page);
  await seq(page, id);
  const box = await screenBox(page);
  const rowPx = (await activePane(page).locator(".xterm-rows > div").first().boundingBox())!.height;
  // The shifted screen is clipped to the terminal's box.
  await expect(activePane(page).locator(".xterm-scrollable-element")).toHaveCSS("overflow", "clip");
  const cdp = await page.context().newCDPSession(page);
  const x = box.x + box.width / 2;
  const at = (y: number) => [{ x, y, id: 1 }];
  let y = box.y + 60;
  await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: at(y) });
  // Past the slop (ours, and Chromium's, which holds back moves within ~15 px), then 3 px at a
  // time: the view follows each 3 px, not whole lines.
  y += 25;
  await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: at(y) });
  let before = await viewPx(page, rowPx);
  const shifts = new Set<number>();
  for (let i = 0; i < 10; i++) {
    y += 3;
    await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: at(y) });
    const now = await viewPx(page, rowPx);
    expect(before - now).toBeCloseTo(3, 0);
    before = now;
    const s = await shift(page);
    // Under half a line (plus rounding) from a whole line.
    expect(Math.abs(s)).toBeLessThanOrEqual(rowPx / 2 + 0.5);
    shifts.add(Math.round(s));
  }
  expect(shifts.size).toBeGreaterThan(3);
  // Held still, then lifted: it eases onto a whole line, nothing left shifted.
  await page.waitForTimeout(120);
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await expect.poll(() => shift(page)).toBe(0);
  expect(await written(page, id)).toBe("");
});

test("ios_dragging_past_the_newest_line_stretches_a_little_and_springs_back", async ({ page }) => {
  const id = await connected(page);
  await seq(page, id);
  const bottom = await firstRow(page);
  const box = await screenBox(page);
  const cdp = await page.context().newCDPSession(page);
  const x = box.x + box.width / 2;
  const at = (y: number) => [{ x, y, id: 1 }];
  // Finger up the screen at the bottom of the scrollback: nothing newer to show.
  await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: at(box.y + 300) });
  for (let i = 1; i <= 10; i++) {
    await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: at(box.y + 300 - 20 * i) });
  }
  const pulled = await shift(page);
  expect(pulled).toBeLessThan(0);
  expect(pulled).toBeGreaterThan(-200);
  expect(await firstRow(page)).toBe(bottom);
  await page.waitForTimeout(120);
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await expect.poll(() => shift(page)).toBe(0);
  expect(await firstRow(page)).toBe(bottom);
});

test("ios_a_fling_keeps_scrolling_after_the_finger_lifts", async ({ page }) => {
  const id = await connected(page);
  await seq(page, id);
  const bottom = await firstRow(page);
  const box = await screenBox(page);
  const rowPx = (await activePane(page).locator(".xterm-rows > div").first().boundingBox())!.height;

  // A quick 150 px flick, 25 px a frame (dispatched in the page so the timing holds under load).
  await page.evaluate(
    async ({ x, y0 }) => {
      const target = document.elementFromPoint(x, y0)!;
      const fire = (type: string, y: number | null) => {
        const touches = y === null ? [] : [new Touch({ identifier: 1, target, clientX: x, clientY: y })];
        const changed = [new Touch({ identifier: 1, target, clientX: x, clientY: y ?? y0 + 150 })];
        target.dispatchEvent(
          new TouchEvent(type, { touches, targetTouches: touches, changedTouches: changed, bubbles: true, cancelable: true }),
        );
      };
      const frame = () => new Promise((r) => setTimeout(r, 16));
      fire("touchstart", y0);
      for (let i = 1; i <= 6; i++) {
        await frame();
        fire("touchmove", y0 + 25 * i);
      }
      fire("touchend", null);
    },
    { x: box.x + box.width / 2, y0: box.y + 60 },
  );
  // Momentum: it settles well past the lines the finger itself covered.
  let last = await firstRow(page);
  for (let i = 0; i < 20; i++) {
    await page.waitForTimeout(250);
    const now = await firstRow(page);
    if (now === last) break;
    last = now;
  }
  const byFinger = Math.round(150 / rowPx);
  expect(bottom - last).toBeGreaterThan(byFinger + 4);
  // It came to rest on a whole line: nothing left shifted.
  await expect.poll(() => shift(page)).toBe(0);
});

test("ios_a_drag_in_the_alternate_screen_scrolls_the_app", async ({ page }) => {
  const id = await connected(page);
  // An app on the alternate screen (less, vim) without mouse reporting: the drag becomes arrow keys.
  await output(page, id, "\x1b[?1049h\x1b[H\x1b[2Jless");
  const box = await screenBox(page);
  const cdp = await page.context().newCDPSession(page);
  await drag(cdp, { x: box.x + box.width / 2, y: box.y + 300 }, { x: box.x + box.width / 2, y: box.y + 100 }, {
    steps: 20,
    stepMs: 30,
  });
  // Finger up the screen: the app scrolls down (Down arrow, several of them).
  await expect.poll(async () => (await written(page, id)).split("\x1b[B").length - 1).toBeGreaterThan(3);
  expect(await written(page, id)).not.toContain("\x1b[A");

  // With mouse reporting on (SGR), wheel reports instead: button 64 (up) for a finger down the screen.
  await output(page, id, "\x1b[?1000h\x1b[?1006h");
  const before = (await written(page, id)).length;
  await drag(cdp, { x: box.x + box.width / 2, y: box.y + 100 }, { x: box.x + box.width / 2, y: box.y + 300 }, {
    steps: 20,
    stepMs: 30,
  });
  await expect.poll(async () => (await written(page, id)).slice(before)).toMatch(/^(\x1b\[<64;\d+;\d+M)+$/);
  // Whole lines only there: the screen is never shifted.
  expect(await shift(page)).toBe(0);
});

test("ios_a_tap_on_the_terminal_still_focuses_it", async ({ page }) => {
  await connected(page);
  const textarea = activePane(page).locator("textarea.xterm-helper-textarea");
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await expect(textarea).not.toBeFocused();
  const box = await screenBox(page);
  await page.touchscreen.tap(box.x + box.width / 2, box.y + box.height / 2);
  await expect(textarea).toBeFocused();
});

test("ios_a_swipe_from_the_left_edge_over_the_terminal_opens_the_drawer", async ({ page }) => {
  const id = await connected(page);
  await seq(page, id);
  const bottom = await firstRow(page);
  const box = await screenBox(page);
  const cdp = await page.context().newCDPSession(page);
  await drag(cdp, { x: 6, y: box.y + 200 }, { x: 160, y: box.y + 210 }, { steps: 10, stepMs: 16 });
  await expect(page.getByRole("dialog", { name: "Menu" })).toBeVisible();
  // A sideways swipe is not a scroll.
  expect(await firstRow(page)).toBe(bottom);
});

test("desktop_terminal_keeps_xterms_own_scrolling", async ({ page }) => {
  const id = await connected(page);
  await expect(activePane(page).locator(".terminal-host")).not.toHaveAttribute("data-touch-scroll", /.*/);
  await seq(page, id);
  const bottom = await firstRow(page);
  // The mouse wheel scrolls the scrollback as before.
  const box = await screenBox(page);
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, -400);
  await expect.poll(() => firstRow(page)).toBeLessThan(bottom);
  // No clipping or shifting of the screen off iOS.
  await expect(activePane(page).locator(".xterm-scrollable-element")).not.toHaveCSS("overflow", "clip");
  expect(await shift(page)).toBe(0);
});
