import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";
import {
  activePane,
  connectHost,
  createVault,
  emulateLinux,
  heldValues,
  holdFields,
  KEYS,
  nav,
  panes,
  seedHost,
  sessionIds,
  terminalPanel,
  terminalTabs,
  workspaceKeys,
  written,
} from "./helpers";

test("open_tab_split_close", async ({ page }) => {
  await createVault(page);

  // A local terminal in a new tab.
  await page.keyboard.press(KEYS.localTerminal);
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect(terminalTabs(page).first()).toContainText("Local shell");
  await expect(panes(page)).toHaveCount(1);
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  const [first] = await sessionIds(page);

  // Keystrokes reach the session as bytes.
  await expect(activePane(page)).toHaveCount(1);
  await page.keyboard.type("ls\r");
  await expect.poll(() => written(page, first)).toBe("ls\r");

  // Split right, then down: each split is a new session on the same target.
  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(2);
  await page.keyboard.press(KEYS.splitDown);
  await expect(panes(page)).toHaveCount(3);
  await expect.poll(() => sessionIds(page)).toHaveLength(3);
  const ids = await panes(page).evaluateAll((els) => els.map((e) => e.getAttribute("data-pane-id")!));
  await expect(activePane(page)).toHaveAttribute("data-pane-id", ids[2]);

  // Focus moves between panes by direction: [0] | ([1] over [2]).
  await page.keyboard.press(KEYS.focus("ArrowLeft"));
  await expect(activePane(page)).toHaveAttribute("data-pane-id", ids[0]);
  await page.keyboard.press(KEYS.focus("ArrowRight"));
  await expect(activePane(page)).toHaveAttribute("data-pane-id", ids[1]);
  await page.keyboard.press(KEYS.focus("ArrowDown"));
  await expect(activePane(page)).toHaveAttribute("data-pane-id", ids[2]);
  await page.keyboard.press(KEYS.focus("ArrowUp"));
  await expect(activePane(page)).toHaveAttribute("data-pane-id", ids[1]);

  // None of the shortcuts reached a shell.
  for (const id of await sessionIds(page)) {
    expect(await written(page, id)).toBe(id === first ? "ls\r" : "");
  }

  // Close the focused pane: its session ends, the others stay.
  await page.keyboard.press(KEYS.close);
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);

  // A new tab offers a launcher; it becomes a terminal.
  await page.keyboard.press(KEYS.newTab);
  await expect(terminalTabs(page)).toHaveCount(2);
  await expect(terminalTabs(page).nth(1)).toHaveAttribute("aria-selected", "true");
  await page.getByRole("tabpanel").getByRole("button", { name: "Local terminal" }).click();
  await expect(panes(page)).toHaveCount(1);
  await expect.poll(() => sessionIds(page)).toHaveLength(3);

  // Closing a tab closes its sessions.
  await page.getByRole("button", { name: "Close tab Local shell" }).nth(1).click();
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  await expect(panes(page)).toHaveCount(2);

  // Closing the last panes leaves the workspace for the hosts page.
  await page.keyboard.press(KEYS.close);
  await page.keyboard.press(KEYS.close);
  await expect(terminalTabs(page)).toHaveCount(0);
  await expect.poll(() => sessionIds(page)).toHaveLength(0);
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
});

test("zoom_keeps_sessions_and_restores", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await page.keyboard.press(KEYS.splitRight);
  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(3);
  await expect.poll(() => sessionIds(page)).toHaveLength(3);
  const before = await sessionIds(page);
  const visible = panes(page).locator("visible=true");

  // Zoom shows the active pane alone.
  await page.keyboard.press(KEYS.zoom);
  await expect(visible).toHaveCount(1);
  await expect(activePane(page)).toBeVisible();
  const zoomed = await activePane(page).getAttribute("data-pane-id");

  // Pane focus does nothing while zoomed: a stashed pane is not a neighbour.
  await page.keyboard.press(KEYS.focus("ArrowLeft"));
  await expect(visible).toHaveCount(1);
  await expect(activePane(page)).toHaveAttribute("data-pane-id", zoomed!);

  await page.keyboard.press(KEYS.zoom);
  await expect(visible).toHaveCount(3);
  expect(await sessionIds(page)).toEqual(before);
});

test("broadcast_types_into_every_open_pane", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await page.keyboard.press(KEYS.splitRight);
  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(3);
  await expect.poll(() => sessionIds(page)).toHaveLength(3);
  await expect(panes(page).locator("visible=true")).toHaveCount(3);
  for (const pane of await panes(page).all()) await expect(pane).toHaveAttribute("data-status", "open");
  const ids = await sessionIds(page);
  const tab = terminalTabs(page).first();

  // Off by default: nothing marks the tab or the panes.
  await expect(panes(page).and(page.locator('[data-broadcast="true"]'))).toHaveCount(0);
  await expect(page.getByRole("tab", { name: /broadcast/i })).toHaveCount(0);

  await page.keyboard.press(KEYS.broadcast);
  // Loud: every pane is outlined and says where typing goes, and the tab says so too.
  await expect(page.locator('.terminal-pane[data-broadcast="true"]')).toHaveCount(3);
  await expect(page.getByText("Typing goes to 3 panes")).toHaveCount(3);
  await expect(tab).toContainText("Broadcast");
  await expect(page.getByRole("tab", { name: /broadcast/i })).toHaveCount(1);

  await page.keyboard.type("uptime\r");
  for (const id of ids) {
    await expect.poll(async () => ((await written(page, id)).match(/uptime\r/g) ?? []).length).toBe(1);
  }
  // Exactly once, however long the others take to settle.
  for (const id of ids) expect(await written(page, id)).toBe("uptime\r");

  // A program's query is answered by its own terminal only: the focused pane's reply is not typing.
  const cursorReplies = async (id: string) => ((await written(page, id)).match(/\x1b\[\d+;\d+R/g) ?? []).length;
  await page.evaluate((all) => {
    for (const id of all) window.__tetherMock.control.output(id, new TextEncoder().encode("\x1b[6n"));
  }, ids);
  for (const id of ids) await expect.poll(() => cursorReplies(id)).toBeGreaterThanOrEqual(1);
  await page.waitForTimeout(300);
  for (const id of ids) expect(await cursorReplies(id)).toBe(1);

  // Off again: only the focused pane gets keys.
  await page.keyboard.press(KEYS.broadcast);
  await expect(page.locator('.terminal-pane[data-broadcast="true"]')).toHaveCount(0);
  await expect(page.getByRole("tab", { name: /broadcast/i })).toHaveCount(0);
  const before = await Promise.all(ids.map((id) => written(page, id)));
  await page.keyboard.type("x");
  await expect.poll(async () => (await Promise.all(ids.map((id) => written(page, id)))).filter((w) => w.endsWith("x")).length).toBe(1);
  const after = await Promise.all(ids.map((id) => written(page, id)));
  expect(after.filter((w, i) => w !== before[i])).toHaveLength(1);
});

test("linux_shortcuts_use_ctrl_shift_and_leave_ctrl_letters_to_the_shell", async ({ page }) => {
  await emulateLinux(page);
  await createVault(page);
  const keys = workspaceKeys(false);

  await page.keyboard.press(keys.localTerminal);
  await expect(panes(page)).toHaveCount(1);
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  const [first] = await sessionIds(page);
  await expect(activePane(page)).toHaveCount(1);

  // Plain Ctrl+T/W/D are shell keys (transpose, delete word, EOF).
  await page.keyboard.press("Control+T");
  await page.keyboard.press("Control+W");
  await page.keyboard.press("Control+D");
  await expect.poll(() => written(page, first)).toBe("\x14\x17\x04");
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect(panes(page)).toHaveCount(1);

  // The Cmd chords mean nothing here.
  await page.keyboard.press("Meta+D");
  await expect(panes(page)).toHaveCount(1);

  // Ctrl+Shift chords drive the workspace and never reach a shell.
  await page.keyboard.press(keys.splitRight);
  await expect(panes(page)).toHaveCount(2);
  await page.keyboard.press(keys.splitDown);
  await expect(panes(page)).toHaveCount(3);
  const ids = await panes(page).evaluateAll((els) => els.map((e) => e.getAttribute("data-pane-id")!));
  await page.keyboard.press(keys.focus("ArrowLeft"));
  await expect(activePane(page)).toHaveAttribute("data-pane-id", ids[0]);
  await page.keyboard.press(keys.close);
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  await page.keyboard.press(keys.newTab);
  await expect(terminalTabs(page)).toHaveCount(2);
  for (const id of await sessionIds(page)) {
    expect(await written(page, id)).toBe("");
  }

  // The tooltips name the Linux chords.
  await expect(page.getByRole("button", { name: "New tab", exact: true })).toHaveAttribute("title", "New tab (Ctrl+Shift+T)");
});

test("unknown_host_key_dialog_accept_saves", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  const key = await page.evaluate(() =>
    window.__tetherMock.control.setServerKey("web1.example", 22, "ssh-ed25519", "AAAAC3NzaC1lZDI1NTE5AAAAIWebOne"),
  );

  await connectHost(page, "web-1");
  const dialog = page.getByRole("dialog", { name: "Unknown host key" });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("web1.example");
  await expect(dialog).toContainText(key.sha256Fingerprint);
  await expect(dialog).toContainText("ssh-ed25519");
  // Nothing is accepted by default.
  await expect(dialog.getByRole("button", { name: "Reject" })).toBeFocused();
  expect(await sessionIds(page)).toEqual([]);

  await dialog.getByRole("button", { name: "Accept & save" }).click();
  await expect(dialog).toBeHidden();
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect(terminalTabs(page).first()).toContainText("web-1");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);

  const known = await page.evaluate(() => window.__tetherMock.listEntities("known_host"));
  expect(known.map((k) => [k.hostPattern, k.port, k.fingerprint])).toEqual([
    ["web1.example", 22, key.sha256Fingerprint],
  ]);

  // Saved: the next connection is not asked about.
  await page.keyboard.press(KEYS.close);
  await expect.poll(() => sessionIds(page)).toHaveLength(0);
  await connectHost(page, "web-1");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("changed_host_key_default_reject", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "db-1", "db1.example");
  const old = await page.evaluate(() =>
    window.__tetherMock.upsertEntity("known_host", {
      hostPattern: "db1.example",
      port: 22,
      algorithm: "ssh-ed25519",
      publicKey: "AAAAC3NzaC1lZDI1NTE5AAAAIOldKey",
    }),
  );
  const fresh = await page.evaluate(() =>
    window.__tetherMock.control.setServerKey("db1.example", 22, "ssh-ed25519", "AAAAC3NzaC1lZDI1NTE5AAAAINewKey"),
  );
  const knownKeys = () =>
    page.evaluate(async () => (await window.__tetherMock.listEntities("known_host")).map((k) => k.publicKey));

  await connectHost(page, "db-1");
  const dialog = page.getByRole("dialog", { name: "Host key changed" });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText(old.fingerprint);
  await expect(dialog).toContainText(fresh.sha256Fingerprint);
  // Reject is the default: Enter rejects.
  const reject = dialog.getByRole("button", { name: "Reject" });
  await expect(reject).toBeFocused();
  const accept = dialog.getByRole("button", { name: "Accept new key" });
  await expect(accept).toBeDisabled();
  await page.keyboard.press("Enter");
  await expect(dialog).toBeHidden();
  const pane = page.getByRole("tabpanel");
  await expect(pane.getByRole("alert")).toContainText("host key rejected");
  expect(await knownKeys()).toEqual(["AAAAC3NzaC1lZDI1NTE5AAAAIOldKey"]);
  expect(await sessionIds(page)).toEqual([]);

  // Escape rejects too.
  await pane.getByRole("button", { name: "Retry" }).click();
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(pane.getByRole("alert")).toContainText("host key rejected");

  // Accepting takes typing "yes".
  await pane.getByRole("button", { name: "Retry" }).click();
  await expect(dialog).toBeVisible();
  const confirm = dialog.getByLabel("Type yes to accept the new key");
  await confirm.fill("ye");
  await expect(accept).toBeDisabled();
  await confirm.fill("yes");
  await expect(accept).toBeEnabled();
  await accept.click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  expect(await knownKeys()).toEqual(["AAAAC3NzaC1lZDI1NTE5AAAAINewKey"]);
});

test("double_click_cannot_accept_an_unseen_host_key", async ({ page }) => {
  await createVault(page);
  await page.evaluate(() => {
    const ask = (host: string, key: string) =>
      window.__tetherMock.control.ask({
        kind: "hostKeyUnknown",
        host,
        port: 22,
        key: { algorithm: "ssh-ed25519", sha256Fingerprint: `SHA256:${key}`, openssh: `ssh-ed25519 ${key}` },
      });
    window.__answers = [ask("first.example", "AAAAfirst"), ask("second.example", "AAAAsecond")];
  });
  const dialog = page.getByRole("dialog", { name: "Unknown host key" });
  await expect(dialog).toContainText("first.example");

  // A double-click on Accept & save: the second click lands on the next
  // dialog's Accept & save (same place), which is not armed yet.
  const accept = dialog.getByRole("button", { name: "Accept & save" });
  await expect(accept).toBeEnabled();
  const box = (await accept.boundingBox())!;
  const [x, y] = [box.x + box.width / 2, box.y + box.height / 2];
  await page.mouse.click(x, y);
  await expect(dialog).toContainText("second.example");
  const nextBox = (await accept.boundingBox())!;
  expect([nextBox.x, nextBox.y]).toEqual([box.x, box.y]);
  await page.mouse.click(x, y);
  expect(await page.evaluate(() => window.__answers![0])).toEqual({ accept: { save: true } });
  await expect(dialog).toContainText("second.example");
  expect(await page.evaluate(() => window.__tetherMock.control.pendingPrompts().length)).toBe(1);

  // It arms shortly after; the user decides.
  await expect(dialog.getByRole("button", { name: "Accept & save" })).toBeEnabled();
  await dialog.getByRole("button", { name: "Reject" }).click();
  await expect(dialog).toBeHidden();
  expect(await page.evaluate(() => window.__answers![1])).toBe("reject");
});

test("passphrase_prompt_flow", async ({ page }) => {
  await createVault(page);
  const secret = "correct-pass-phrase-77";

  await page.evaluate(() => {
    window.__answer = window.__tetherMock.control.ask({ kind: "passphrase", keyLabel: "deploy key" });
  });
  const dialog = page.getByRole("dialog", { name: "Key passphrase" });
  await expect(dialog).toContainText("deploy key");
  const field = dialog.getByLabel("Passphrase");
  await expect(field).toBeFocused();
  await field.fill(secret);
  await holdFields(field);
  await page.keyboard.press("Enter");
  await expect(dialog).toBeHidden();
  expect(await page.evaluate(() => window.__answer)).toEqual({ secret });
  // The secret is not kept: not in the page, not in the (detached) field.
  expect(await page.content()).not.toContain(secret);
  expect(await heldValues(page)).toEqual([""]);

  // Cancel (Escape) answers reject.
  await page.evaluate(() => {
    window.__answer = window.__tetherMock.control.ask({ kind: "passphrase", keyLabel: "deploy key" });
  });
  await expect(dialog).toBeVisible();
  await field.fill("typed-then-cancelled");
  await holdFields(field);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  expect(await page.evaluate(() => window.__answer)).toBe("reject");
  expect(await heldValues(page)).toEqual([""]);

  // A password prompt that times out in the backend goes away.
  await page.evaluate(() => {
    window.__answer = window.__tetherMock.control.ask({ kind: "password", user: "deploy", host: "web1.example", saveTo: null });
  });
  const password = page.getByRole("dialog", { name: "Password" });
  await expect(password).toContainText("deploy@web1.example");
  await password.getByLabel("Password").fill("typed-but-withdrawn");
  await holdFields(password.getByLabel("Password"));
  await page.evaluate(() => window.__tetherMock.control.withdraw(window.__tetherMock.control.pendingPrompts()[0]));
  await expect(password).toBeHidden();
  expect(await page.evaluate(() => window.__answer)).toBe("reject");
  expect(await page.content()).not.toContain("typed-but-withdrawn");
  expect(await heldValues(page)).toEqual([""]);
});

test("keyboard_interactive_prompt_flow", async ({ page }) => {
  await createVault(page);
  await page.evaluate(() => {
    window.__answer = window.__tetherMock.control.ask({
      kind: "keyboardInteractive",
      name: "Two-factor login",
      instructions: "Answer both questions.",
      prompts: [
        ["Password:", false],
        ["Favourite colour:", true],
      ],
      saveTo: { identityLabel: "alice", isNew: false },
    });
  });
  const dialog = page.getByRole("dialog", { name: "Two-factor login" });
  await expect(dialog).toContainText("Answer both questions.");
  // Two answers (a password and something else): saving is not offered.
  await expect(dialog.getByRole("checkbox")).toHaveCount(0);
  const hidden = dialog.getByLabel("Password:");
  const shown = dialog.getByLabel("Favourite colour:");
  await expect(hidden).toHaveAttribute("type", "password");
  await expect(shown).toHaveAttribute("type", "text");
  await hidden.fill("hunter2-secret");
  await shown.fill("blue");
  await holdFields(hidden, shown);
  await dialog.getByRole("button", { name: "Continue" }).click();
  await expect(dialog).toBeHidden();
  expect(await page.evaluate(() => window.__answer)).toEqual({ answers: ["hunter2-secret", "blue"] });
  expect(await page.content()).not.toContain("hunter2-secret");
  expect(await heldValues(page)).toEqual(["", ""]);
});

test("save_password_checkbox_sends_save_secret", async ({ page }) => {
  await createVault(page);
  // On an existing identity: unchecked by default, says where it goes.
  await page.evaluate(() => {
    window.__answer = window.__tetherMock.control.ask({
      kind: "password",
      user: "deploy",
      host: "web1.example",
      saveTo: { identityLabel: "alice", isNew: false },
    });
  });
  const dialog = page.getByRole("dialog", { name: "Password" });
  const save = dialog.getByRole("checkbox", { name: "Save password to vault" });
  await expect(save).not.toBeChecked();
  await expect(dialog).toContainText("on identity “alice”");
  await dialog.getByLabel("Password", { exact: true }).fill("kept-secret");
  await save.check();
  await dialog.getByRole("button", { name: "Continue" }).click();
  await expect(dialog).toBeHidden();
  expect(await page.evaluate(() => window.__answer)).toEqual({ saveSecret: "kept-secret" });
  expect(await page.content()).not.toContain("kept-secret");

  // A new identity, left unchecked: a plain secret.
  await page.evaluate(() => {
    window.__answer = window.__tetherMock.control.ask({
      kind: "password",
      user: "deploy",
      host: "web1.example",
      saveTo: { identityLabel: "web1", isNew: true },
    });
  });
  await expect(dialog).toContainText("as this host's own login");
  await expect(save).not.toBeChecked();
  await dialog.getByLabel("Password", { exact: true }).fill("not-kept");
  await dialog.getByRole("button", { name: "Continue" }).click();
  expect(await page.evaluate(() => window.__answer)).toEqual({ secret: "not-kept" });

  // A single hidden keyboard-interactive prompt offers it too.
  await page.evaluate(() => {
    window.__answer = window.__tetherMock.control.ask({
      kind: "keyboardInteractive",
      name: "",
      instructions: "",
      prompts: [["Password:", false]],
      saveTo: { identityLabel: "alice", isNew: false },
    });
  });
  const kbd = page.getByRole("dialog", { name: "Authentication" });
  await kbd.getByLabel("Password:").fill("kbd-secret");
  await kbd.getByRole("checkbox", { name: "Save password to vault" }).check();
  await kbd.getByRole("button", { name: "Continue" }).click();
  expect(await page.evaluate(() => window.__answer)).toEqual({ saveAnswers: ["kbd-secret"] });
});

test("no_save_checkbox_without_offer", async ({ page }) => {
  await createVault(page);
  await page.evaluate(() => {
    window.__answer = window.__tetherMock.control.ask({ kind: "password", user: "u", host: "h", saveTo: null });
  });
  const dialog = page.getByRole("dialog", { name: "Password" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("checkbox")).toHaveCount(0);
});

test("broadcast_badge_stays_whole_on_a_long_tab_name", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "a-very-long-host-label-for-a-tab", "10.0.0.5");
  await connectHost(page, "a-very-long-host-label-for-a-tab");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(2);
  await page.keyboard.press(KEYS.broadcast);

  const tab = terminalTabs(page).first();
  const badge = tab.locator(".tab-broadcast");
  await expect(badge).toBeVisible();
  const label = tab.locator(".tab-label");
  // The name is the part that is cut short; the badge is whole and inside the tab.
  expect(await label.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true);
  const [t, b] = [(await tab.boundingBox())!, (await badge.boundingBox())!];
  expect(b.x).toBeGreaterThanOrEqual(t.x);
  expect(b.x + b.width).toBeLessThanOrEqual(t.x + t.width + 0.5);
  expect(await badge.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
});

test("rename_tab_by_double_click", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web", "10.0.0.5");
  await connectHost(page, "web");
  await connectHost(page, "web");
  // Two tabs on one host are numbered.
  await expect(terminalTabs(page)).toHaveText(["web", "web (2)"]);

  const second = terminalTabs(page).nth(1);
  await second.dblclick();
  const name = page.getByRole("textbox", { name: "Tab name" });
  await expect(name).toHaveValue("web (2)");
  await name.fill("deploy logs");
  await name.press("Enter");
  await expect(terminalTabs(page)).toHaveText(["web", "deploy logs"]);
  await expect(page.getByRole("button", { name: "Close tab deploy logs" })).toBeVisible();

  // Escape keeps the name; a blank name goes back to the automatic one.
  await terminalTabs(page).nth(1).dblclick();
  await name.fill("never mind");
  await name.press("Escape");
  await expect(terminalTabs(page)).toHaveText(["web", "deploy logs"]);
  await terminalTabs(page).nth(1).dblclick();
  await name.fill("");
  await name.press("Enter");
  await expect(terminalTabs(page)).toHaveText(["web", "web (2)"]);
});

test("drag_tab_reorders_without_remount", async ({ page }) => {
  await createVault(page);
  for (const name of ["one", "two", "three"]) {
    await page.keyboard.press(KEYS.localTerminal);
    await expect(terminalTabs(page)).toHaveCount(name === "one" ? 1 : name === "two" ? 2 : 3);
    await terminalTabs(page).last().dblclick();
    const field = page.getByRole("textbox", { name: "Tab name" });
    await field.fill(name);
    await field.press("Enter");
  }
  await expect(terminalTabs(page)).toHaveText(["one", "two", "three"]);
  await expect.poll(() => sessionIds(page)).toHaveLength(3);
  const sessions = await sessionIds(page);
  // "three" is the selected tab; dragging another must not change that.
  await expect(terminalTabs(page).nth(2)).toHaveAttribute("aria-selected", "true");

  // Record every tab panel the page removes: a reorder must not remount.
  await page.evaluate(() => {
    const w = window as unknown as { __removedPanels: number };
    w.__removedPanels = 0;
    const root = document.querySelector('section[aria-label="Terminals"]')!;
    new MutationObserver((records) => {
      for (const r of records) {
        for (const n of r.removedNodes) {
          if (n instanceof HTMLElement && n.matches('[role="tabpanel"]')) w.__removedPanels++;
        }
      }
    }).observe(root, { childList: true });
  });

  // Drop "three" on the left half of "one": it goes first.
  const box = (await terminalTabs(page).nth(0).locator("xpath=..").boundingBox())!;
  const leftHalf = { x: 2, y: box.height / 2 };
  await terminalTabs(page).nth(2).dragTo(terminalTabs(page).nth(0), { targetPosition: leftHalf });
  await expect(terminalTabs(page)).toHaveText(["three", "one", "two"]);
  // Dragging an inactive tab ("two", onto the left of "one") does not select it.
  await terminalTabs(page).nth(2).dragTo(terminalTabs(page).nth(1), { targetPosition: leftHalf });
  await expect(terminalTabs(page)).toHaveText(["three", "two", "one"]);
  // Drop "three" on the right half of the last tab: it goes last.
  const lastBox = (await terminalTabs(page).nth(2).boundingBox())!;
  const rightHalf = { x: lastBox.width - 2, y: lastBox.height / 2 };
  await terminalTabs(page).nth(0).dragTo(terminalTabs(page).nth(2), { targetPosition: rightHalf });
  await expect(terminalTabs(page)).toHaveText(["two", "one", "three"]);

  expect(await page.evaluate(() => (window as unknown as { __removedPanels: number }).__removedPanels)).toBe(0);
  expect(await sessionIds(page)).toEqual(sessions);
  await expect(page.getByRole("tab", { name: "three" })).toHaveAttribute("aria-selected", "true");
  await expect(page.locator(".tab[data-drop-before], .tab[data-drop-after]")).toHaveCount(0);
});

test("split_shows_pane_headers_with_focus", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web", "10.0.0.5");
  await connectHost(page, "web");
  await expect(panes(page)).toHaveCount(1);
  // One pane: no header.
  await expect(page.getByRole("tabpanel").locator(".pane-header")).toHaveCount(0);

  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(2);
  const headers = page.getByRole("tabpanel").locator(".pane-header");
  await expect(headers).toHaveText(["web", "web (2)"]);
  await expect(activePane(page).locator(".pane-header")).toHaveText("web (2)");
  await expect(terminalTabs(page)).toHaveText(["web (2)"]);

  await panes(page).first().click();
  await expect(activePane(page).locator(".pane-header")).toHaveText("web");
  await expect(terminalTabs(page)).toHaveText(["web"]);

  // Opening the name editor and leaving it unchanged does not pin the
  // automatic name: the tab still follows the focused pane.
  await terminalTabs(page).first().dblclick();
  await page.getByRole("textbox", { name: "Tab name" }).press("Enter");
  await panes(page).nth(1).click();
  await expect(terminalTabs(page)).toHaveText(["web (2)"]);
});

test("layout_menu_grid_then_pick_hosts", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await seedHost(page, "web-2", "web2.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  const [first] = await sessionIds(page);

  // The grid keeps web-1 and adds three launcher panes.
  await page.getByRole("button", { name: "Layout", exact: true }).click();
  await page.getByRole("menuitem", { name: "2×2 grid" }).click();
  await expect(panes(page)).toHaveCount(4);
  const launchers = panes(page).and(page.locator('[data-launcher="true"]'));
  await expect(launchers).toHaveCount(3);

  // Picking a host in one of them opens it there.
  await launchers.first().getByRole("button", { name: /^web-2/ }).click();
  await expect(launchers).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  expect((await sessionIds(page))[0]).toBe(first);
});

test("new_tab_then_grid_gives_four_pickers", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await page.keyboard.press(KEYS.newTab);
  await expect(terminalTabs(page)).toHaveCount(1);
  await page.getByRole("button", { name: "Layout", exact: true }).click();
  await page.getByRole("menuitem", { name: "2×2 grid" }).click();
  const launchers = panes(page).and(page.locator('[data-launcher="true"]'));
  await expect(launchers).toHaveCount(4);
  await launchers.nth(2).getByRole("button", { name: /^web-1/ }).click();
  await expect(launchers).toHaveCount(3);
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
});

test("a_preset_on_a_new_tab_focuses_the_first_host_of_its_first_launcher", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await seedHost(page, "web-2", "web2.example");
  await page.keyboard.press(KEYS.newTab);
  await expect(terminalTabs(page)).toHaveCount(1);
  await page.getByRole("button", { name: "Layout", exact: true }).click();
  await page.getByRole("menuitem", { name: "2×2 grid" }).click();
  const launchers = panes(page).and(page.locator('[data-launcher="true"]'));
  await expect(launchers).toHaveCount(4);
  await expect(launchers.first().getByRole("button", { name: /^web-1/ })).toBeFocused();
  // The keyboard works from there: Enter opens the host in that pane.
  await page.keyboard.press("Enter");
  await expect(launchers).toHaveCount(3);
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
});

test("layout_menu_gives_focus_back", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  await expect(panes(page).and(page.locator('[data-status="open"]'))).toHaveCount(2);
  const layout = page.getByRole("button", { name: "Layout", exact: true });

  // Escape: back on the button that opened the menu.
  await layout.click();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(layout).toBeFocused();

  // A choice that leaves the active pane alone: typing still reaches the session.
  await layout.click();
  await page.getByRole("menuitemcheckbox", { name: /Broadcast input/ }).click();
  await expect(page.getByRole("tab", { name: /broadcast/i })).toHaveCount(1);
  await page.keyboard.type("ls");
  for (const id of await sessionIds(page)) await expect.poll(() => written(page, id)).toBe("ls");
});

test("pane_menu_maximize_and_move_to_tab", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
  const before = await sessionIds(page);
  const visible = panes(page).locator("visible=true");
  const paneMenu = page.getByRole("button", { name: "Pane menu for web-1 (2)" });

  await paneMenu.click();
  await page.getByRole("menuitem", { name: "Maximize pane" }).click();
  await expect(visible).toHaveCount(1);
  await expect(page.locator(".pane-header").locator("visible=true")).toHaveText(/web-1 \(2\) · Maximized · 2 panes/);

  await page.getByRole("button", { name: "Restore", exact: true }).click();
  await expect(visible).toHaveCount(2);

  // The pane leaves for a tab of its own, its session untouched.
  await paneMenu.click();
  await page.getByRole("menuitem", { name: "Move to new tab" }).click();
  await expect(terminalTabs(page)).toHaveCount(2);
  await expect(panes(page)).toHaveCount(1);
  expect(await sessionIds(page)).toEqual(before);
});

test("pane_menu_opens_on_right_click", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await page.keyboard.press(KEYS.splitRight);
  await expect(panes(page)).toHaveCount(2);

  // Right-clicking the first pane's header menus that pane (and makes it the active one).
  await panes(page).first().locator(".pane-header").click({ button: "right" });
  await expect(activePane(page)).toHaveAttribute("data-pane-id", (await panes(page).first().getAttribute("data-pane-id"))!);
  await expect(page.getByRole("menu", { name: "Pane menu" })).toBeVisible();
  await page.getByRole("menuitem", { name: "Close pane" }).click();
  await expect(panes(page)).toHaveCount(1);
});

test("theme_switch_applies_palette", async ({ page }) => {
  await createVault(page);
  const rootVar = (name: string) =>
    page.evaluate((n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim(), name);
  expect(await rootVar("--bg")).toBe("#121418"); // Termlyx Dark

  await page.keyboard.press(KEYS.localTerminal);
  await expect(panes(page)).toHaveCount(1);
  await expect(panes(page).first()).toHaveAttribute("data-terminal-theme", "tether-dark");

  await nav(page).getByRole("button", { name: "Settings", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeVisible();
  const theme = page.getByLabel("Theme", { exact: true });
  for (const name of ["Termlyx Dark", "Termlyx Light", "Dracula", "Nord"]) {
    await expect(theme.getByRole("option", { name, exact: true })).toHaveCount(1);
  }
  await theme.selectOption({ label: "Termlyx Light" });
  // The app's colours ...
  expect(await rootVar("--bg")).toBe("#f6f7f9");
  expect(await rootVar("--text")).toBe("#1d2129");
  await page.getByLabel("Font size").fill("16");

  // ... and the open terminal's.
  await nav(page).getByRole("button", { name: /^Terminals/ }).click();
  const pane = panes(page).first();
  await expect(pane).toHaveAttribute("data-terminal-theme", "tether-light");
  await expect(pane).toHaveCSS("background-color", "rgb(251, 251, 252)");
  await expect(pane).toHaveAttribute("data-font-size", "16");

  // The choice survives a reload.
  await page.reload();
  await expect(page.getByRole("heading", { name: "Create your vault" })).toBeVisible();
  expect(await rootVar("--bg")).toBe("#f6f7f9");
});

test("quit_with_active_sessions_prompts", async ({ page }) => {
  await createVault(page);
  const hostId = await seedHost(page, "files-1", "files.example");
  await page.keyboard.press(KEYS.localTerminal);
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await page.evaluate(async (id) => {
    const api = window.__tetherMock;
    await api.sftpOpen(id);
    await api.transferEnqueue(id, {
      direction: "upload",
      local: "/Users/me/report.pdf",
      remote: "/srv/report.pdf",
      policy: "ask",
    });
  }, hostId);
  const closed = () => page.evaluate(() => window.__tetherMock.control.windowClosed());

  // Closing the window lists what would be lost, and Cancel keeps it all.
  await page.evaluate(() => window.__tetherMock.control.requestClose());
  const dialog = page.getByRole("dialog", { name: "Quit Termlyx?" });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("Local shell");
  await expect(dialog).toContainText("/Users/me/report.pdf");
  await expect(dialog).toContainText("files-1");
  const cancel = dialog.getByRole("button", { name: "Cancel", exact: true });
  await expect(cancel).toBeFocused();
  await cancel.click();
  await expect(dialog).toBeHidden();
  expect(await closed()).toBe(false);
  expect(await sessionIds(page)).toHaveLength(1);

  // Escape is Cancel too.
  await page.evaluate(() => window.__tetherMock.control.requestClose());
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  expect(await closed()).toBe(false);

  await page.evaluate(() => window.__tetherMock.control.requestClose());
  await dialog.getByRole("button", { name: "Quit anyway" }).click();
  await expect.poll(closed).toBe(true);

  // "Cancel transfers" stops the transfers and keeps the app and its sessions.
  await page.reload();
  await createVault(page);
  const keepHost = await seedHost(page, "files-2", "files.example");
  await page.keyboard.press(KEYS.localTerminal);
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  await page.evaluate(async (id) => {
    const api = window.__tetherMock;
    await api.sftpOpen(id);
    await api.transferEnqueue(id, { direction: "upload", local: "/Users/me/a.iso", remote: "/srv/a.iso", policy: "ask" });
  }, keepHost);
  await page.evaluate(() => window.__tetherMock.control.requestClose());
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel transfers" }).click();
  await expect(dialog).toBeHidden();
  expect(await closed()).toBe(false);
  await expect
    .poll(() => page.evaluate(async () => (await window.__tetherMock.activeWork()).transfers.length))
    .toBe(0);
  expect(await sessionIds(page)).toHaveLength(1);
  // With only a session left, the dialog offers no transfer button.
  await page.evaluate(() => window.__tetherMock.control.requestClose());
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Cancel transfers" })).toHaveCount(0);
  await dialog.getByRole("button", { name: "Quit anyway" }).click();
  await expect.poll(closed).toBe(true);

  // A running port forward alone is live work too.
  await page.reload();
  await createVault(page);
  const tunnelHost = await seedHost(page, "tunnel-1", "tunnel.example");
  const ruleId = await page.evaluate(async (hostId) => {
    const api = window.__tetherMock;
    const rule = await api.upsertEntity("forward_rule", {
      hostId,
      label: "db tunnel",
      kind: "local",
      bindAddr: "127.0.0.1",
      bindPort: 15432,
      destHost: "db.internal",
      destPort: 5432,
      autostart: false,
    });
    await api.forwardStart(rule.id);
    return rule.id;
  }, tunnelHost);
  await page.evaluate(() => window.__tetherMock.control.requestClose());
  await expect(dialog).toBeVisible();
  const forwards = dialog.getByRole("list", { name: "Running port forwards" });
  await expect(forwards).toContainText("db tunnel on tunnel-1");
  await expect(forwards).toContainText("127.0.0.1:15432");
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(dialog).toBeHidden();
  expect(await closed()).toBe(false);
  // Once it is stopped, nothing is live.
  await page.evaluate((id) => window.__tetherMock.forwardStop(id), ruleId);
  await page.evaluate(() => window.__tetherMock.control.requestClose());
  await expect.poll(closed).toBe(true);
  await expect(page.getByRole("dialog")).toHaveCount(0);

  // Nothing live: the window just closes.
  await page.reload();
  await createVault(page);
  await page.evaluate(() => window.__tetherMock.control.requestClose());
  await expect.poll(closed).toBe(true);
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("local_shell_picker", async ({ page }) => {
  await page.addInitScript(() => {
    window.__tetherMockSeed = {
      localShells: [
        { id: "powershell", label: "Windows PowerShell" },
        { id: "cmd", label: "Command Prompt" },
        { id: "wsl:Ubuntu", label: "WSL: Ubuntu" },
      ],
    };
  });
  const select = (p: typeof page) => p.getByRole("combobox", { name: "Local shell", exact: true });
  const openSettings = (p: typeof page) => nav(p).getByRole("button", { name: "Settings", exact: true }).click();
  await createVault(page);

  // Nothing chosen: a new local terminal asks for the default shell.
  await page.keyboard.press(KEYS.localTerminal);
  await expect(terminalTabs(page)).toHaveCount(1);
  await openSettings(page);
  await expect(select(page).locator("option")).toHaveText([
    "Windows PowerShell (default)",
    "Command Prompt",
    "WSL: Ubuntu",
  ]);

  // The choice applies to the next new local terminal.
  await select(page).selectOption({ label: "WSL: Ubuntu" });
  await page.keyboard.press(KEYS.localTerminal);
  await expect(terminalTabs(page)).toHaveCount(2);
  await expect
    .poll(() => page.evaluate(() => window.__tetherMock.control.openedLocalShells()))
    .toEqual([null, "wsl:Ubuntu"]);

  // It is remembered on this device.
  await page.reload();
  await createVault(page);
  await openSettings(page);
  await expect(select(page)).toHaveValue("wsl:Ubuntu");
});

test("local_shell_picker_hidden_with_one_shell", async ({ page }) => {
  await createVault(page);
  await nav(page).getByRole("button", { name: "Settings", exact: true }).click();
  await expect(page.getByLabel("Font size")).toBeVisible();
  await expect(page.getByRole("combobox", { name: "Local shell", exact: true })).toHaveCount(0);
});

/** web-1 open, then web-2 split in on its right: two panes, web-1 left. */
async function twoPanes(page: Page) {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await seedHost(page, "web-2", "web2.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  await page.getByRole("button", { name: "Layout", exact: true }).click();
  await page.getByRole("menuitem", { name: "Split right…" }).click();
  const pick = page.getByRole("dialog", { name: "Open in new pane" });
  await pick.getByRole("combobox", { name: "Search" }).fill("web-2");
  await pick.getByRole("combobox", { name: "Search" }).press("Enter");
  await expect(panes(page)).toHaveCount(2);
  await expect.poll(() => sessionIds(page)).toHaveLength(2);
}

/** Drags the (first) divider between side-by-side panes so the left column is `fraction` of the tab's width. */
async function dragDivider(page: Page, fraction: number) {
  const area = (await terminalPanel(page).boundingBox())!;
  const handle = (await terminalPanel(page).locator('.split-handle[aria-orientation="vertical"]').first().boundingBox())!;
  const y = handle.y + handle.height / 2;
  await page.mouse.move(handle.x + handle.width / 2, y);
  await page.mouse.down();
  await page.mouse.move(area.x + area.width * fraction, y, { steps: 8 });
  await page.mouse.up();
}

test("drag_header_swaps_panes_without_remount", async ({ page }) => {
  await twoPanes(page);
  const before = await sessionIds(page);
  const header = (i: number) => panes(page).nth(i).locator(".pane-header");
  await expect(header(0)).toHaveText("web-1");
  await expect(header(1)).toHaveText("web-2");

  await header(0).dragTo(panes(page).nth(1));
  await expect(header(0)).toHaveText("web-2");
  await expect(header(1)).toHaveText("web-1");
  expect(await sessionIds(page)).toEqual(before);
  // Nothing is left marked as a target.
  await expect(page.locator("[data-drop-target]")).toHaveCount(0);

  // A tab dragged over a pane is not a pane drop.
  await terminalTabs(page).first().dragTo(panes(page).nth(1));
  await expect(header(0)).toHaveText("web-2");
});

test("double_click_divider_equalizes", async ({ page }) => {
  await twoPanes(page);
  await dragDivider(page, 0.25);
  const widths = async () => [(await panes(page).first().boundingBox())!.width, (await panes(page).last().boundingBox())!.width];
  const [l, r] = await widths();
  expect(r - l).toBeGreaterThan(100);

  await terminalPanel(page).locator(".split-handle").dblclick();
  await expect.poll(async () => Math.abs((await widths())[0] - (await widths())[1])).toBeLessThanOrEqual(2);
});

test("zoom_restore_keeps_dragged_sizes", async ({ page }) => {
  await twoPanes(page);
  await dragDivider(page, 0.3);
  const width = async () => (await panes(page).first().boundingBox())!.width;
  const before = await width();
  const visible = panes(page).locator("visible=true");

  await page.keyboard.press(KEYS.zoom);
  await expect(visible).toHaveCount(1);
  await page.keyboard.press(KEYS.zoom);
  await expect(visible).toHaveCount(2);
  await expect.poll(async () => Math.abs((await width()) - before)).toBeLessThanOrEqual(3);
});

test("reapplying_a_preset_resets_dragged_sizes", async ({ page }) => {
  await twoPanes(page);
  const layout = page.getByRole("button", { name: "Layout", exact: true });
  const widths = async () => [(await panes(page).first().boundingBox())!.width, (await panes(page).last().boundingBox())!.width];
  // The first application makes the preset's own splits; drag its divider.
  await layout.click();
  await page.getByRole("menuitem", { name: "2 columns" }).click();
  await dragDivider(page, 0.3);
  const [l, r] = await widths();
  expect(r - l).toBeGreaterThan(100);

  // Choosing it again starts equal, not at the dragged sizes.
  await layout.click();
  await page.getByRole("menuitem", { name: "2 columns" }).click();
  await expect.poll(async () => Math.abs((await widths())[0] - (await widths())[1])).toBeLessThanOrEqual(2);
});

test("swap_keeps_slot_sizes", async ({ page }) => {
  await twoPanes(page);
  await dragDivider(page, 0.3);
  const width = async () => (await panes(page).first().boundingBox())!.width;
  const before = await width();
  const ids = await sessionIds(page);

  await panes(page).first().locator(".pane-header").dragTo(panes(page).nth(1));
  await expect(panes(page).first().locator(".pane-header")).toHaveText("web-2");
  // The geometry stays; only the panes trade places.
  await expect.poll(async () => Math.abs((await width()) - before)).toBeLessThanOrEqual(3);
  expect(await sessionIds(page)).toEqual(ids);
});

test("swap_across_groups_keeps_slot_sizes", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "web1.example");
  await connectHost(page, "web-1");
  await expect(activePane(page)).toHaveAttribute("data-status", "open");
  // H[a, V[b, c]]: split right, then the new pane down.
  await page.keyboard.press(KEYS.splitRight);
  await page.keyboard.press(KEYS.splitDown);
  await expect(panes(page)).toHaveCount(3);
  await expect.poll(() => sessionIds(page)).toHaveLength(3);
  const ids = await sessionIds(page);

  await dragDivider(page, 0.3);
  const box = async (i: number) => (await panes(page).nth(i).boundingBox())!;
  const [a, b, c] = [await box(0), await box(1), await box(2)];
  const titles = await panes(page).locator(".pane-header").allTextContents();

  // a (the left column) trades with c (the bottom of the right column).
  await panes(page).nth(0).locator(".pane-header").dragTo(panes(page).nth(2));
  await expect(panes(page).nth(0).locator(".pane-header")).toHaveText(titles[2]);
  await expect(panes(page).nth(2).locator(".pane-header")).toHaveText(titles[0]);
  for (const [i, was] of [a, b, c].entries()) {
    await expect
      .poll(async () => {
        const now = await box(i);
        return Math.max(Math.abs(now.width - was.width), Math.abs(now.height - was.height));
      })
      .toBeLessThanOrEqual(3);
  }
  expect(await sessionIds(page)).toEqual(ids);
});

test("swap_drag_swap_back_keeps_slot_sizes", async ({ page }) => {
  await twoPanes(page);
  const ids = await sessionIds(page);
  const width = async () => (await panes(page).first().boundingBox())!.width;
  const swap = async (first: string) => {
    await panes(page).first().locator(".pane-header").dragTo(panes(page).nth(1));
    await expect(panes(page).first().locator(".pane-header")).not.toHaveText(first);
  };

  await dragDivider(page, 0.3);
  await swap("web-1");
  await dragDivider(page, 0.6);
  await expect.poll(width).toBeGreaterThan(500);
  const before = await width();
  // Back to the first ordering, whose sizes the library saw before the drag.
  await swap("web-2");
  await expect.poll(async () => Math.abs((await width()) - before)).toBeLessThanOrEqual(3);
  expect(await sessionIds(page)).toEqual(ids);
});
