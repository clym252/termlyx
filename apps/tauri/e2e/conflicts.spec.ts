import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";
import { PASSWORD, createVault, nav } from "./helpers";

const badge = (page: Page) => page.getByTestId("conflict-badge");
const card = (page: Page, title: string) => page.getByRole("article", { name: title });

interface Seed {
  kind: "host" | "identity" | "ssh_key" | "group";
  current: Record<string, unknown> | null;
  other: Record<string, unknown> | null;
  detectedAt?: number;
}

/** Adds conflicts to the mock and lets the app hear of them the way it does: a sync status event. */
async function addConflicts(page: Page, ...seeds: Seed[]): Promise<string[]> {
  const ids = await page.evaluate((seeds) => {
    const mock = window.__tetherMock.control;
    const ids = seeds.map((s) => mock.addConflict(s).id);
    mock.setSyncStatus({ state: "idle", lastSync: Date.now() });
    return ids;
  }, seeds);
  return ids;
}

const resolved = (page: Page) => page.evaluate(() => window.__tetherMock.control.resolvedConflicts());

async function openConflicts(page: Page) {
  await badge(page).click();
  await expect(page.getByRole("heading", { name: "Conflicts", level: 1 })).toBeVisible();
}

const hostConflict = (): Seed => ({
  kind: "host",
  current: { id: "h1", label: "prod-db", address: "10.0.0.5", group_id: null, tag_ids: [], settings: { port: 22 } },
  other: { id: "h1", label: "prod-db", address: "10.0.0.9", group_id: null, tag_ids: [], settings: { port: 2222 } },
});

test("conflict_badge_counts", async ({ page }) => {
  await createVault(page);
  // None: no badge, and the nav has no Conflicts entry either.
  await expect(badge(page)).toHaveCount(0);
  await expect(nav(page).getByRole("button", { name: /conflict/i })).toHaveCount(0);

  await addConflicts(page, hostConflict());
  await expect(badge(page)).toHaveText("1 conflict");

  await addConflicts(page, {
    kind: "identity",
    current: { id: "i1", label: "deploy", username: "root", password: "a" },
    other: { id: "i1", label: "deploy", username: "root", password: "b" },
  });
  await expect(badge(page)).toHaveText("2 conflicts");

  await openConflicts(page);
  await expect(badge(page)).toHaveAttribute("aria-current", "page");
  await card(page, "Host · prod-db").getByRole("button", { name: "Keep current" }).click();
  await expect(badge(page)).toHaveText("1 conflict");
  await card(page, "Identity · deploy").getByRole("button", { name: "Keep current" }).click();
  // The other version holds a password: dropping it asks first.
  await page.getByRole("dialog", { name: "Discard the other version's password?" }).getByRole("button", { name: "Discard" }).click();
  // Resolved: the badge goes away, the page says so.
  await expect(badge(page)).toHaveCount(0);
  await expect(page.getByText("No conflicts.")).toBeVisible();
});

test("conflicts_arriving_while_the_page_is_open_appear", async ({ page }) => {
  await createVault(page);
  await addConflicts(page, hostConflict());
  await openConflicts(page);
  await expect(page.getByRole("article")).toHaveCount(1);
  await addConflicts(page, {
    kind: "group",
    current: { id: "g1", name: "Production", parent_id: null },
    other: { id: "g1", name: "Prod", parent_id: null },
  });
  await expect(page.getByRole("article")).toHaveCount(2);
  await expect(card(page, "Group · Production")).toBeVisible();
});

test("keep_current_dismisses", async ({ page }) => {
  await createVault(page);
  const [id] = await addConflicts(page, hostConflict());
  await openConflicts(page);

  const c = card(page, "Host · prod-db");
  // Side by side: both versions, the changed field marked.
  await expect(c.getByRole("columnheader", { name: "Current version" })).toBeVisible();
  await expect(c.getByRole("columnheader", { name: "Other version" })).toBeVisible();
  const address = c.getByRole("row", { name: /Address/ });
  await expect(address).toContainText("10.0.0.5");
  await expect(address).toContainText("10.0.0.9");
  await expect(address).toHaveAttribute("data-changed", "true");
  await expect(c.getByRole("row", { name: /Label/ })).toHaveAttribute("data-changed", "false");

  await c.getByRole("button", { name: "Keep current" }).click();
  await expect(c).toHaveCount(0);
  expect(await resolved(page)).toEqual([{ id, resolution: { keepCurrent: true } }]);
});

test("use_other_version", async ({ page }) => {
  await createVault(page);
  const [id] = await addConflicts(page, hostConflict());
  await openConflicts(page);
  await card(page, "Host · prod-db").getByRole("button", { name: "Use other version" }).click();
  await expect(card(page, "Host · prod-db")).toHaveCount(0);
  expect(await resolved(page)).toEqual([{ id, resolution: { useOther: true } }]);
});

test("pick_fields_applies_selection", async ({ page }) => {
  await createVault(page);
  const group = await page.evaluate(async () => (await window.__tetherMock.upsertEntity("group", { name: "Production" })).id);
  const [id] = await addConflicts(page, {
    kind: "host",
    current: { id: "h1", label: "prod-db", address: "10.0.0.5", group_id: null, tag_ids: [], settings: { port: 22 } },
    other: { id: "h1", label: "db-1", address: "10.0.0.9", group_id: group, tag_ids: [], settings: { port: 2222 } },
  });
  await openConflicts(page);

  const c = card(page, "Host · prod-db");
  // References show the record's name; settings flatten to labelled lines.
  await expect(c.getByRole("row", { name: /Group/ })).toContainText("Production");
  await expect(c.getByRole("row", { name: /Settings/ })).toContainText("Port: 22");
  await expect(c.getByRole("row", { name: /Settings/ })).toContainText("Port: 2222");

  await c.getByRole("button", { name: "Pick fields" }).click();
  const apply = c.getByRole("button", { name: "Apply" });
  await expect(apply).toBeDisabled();
  // Only changed fields can be picked.
  await expect(c.getByRole("checkbox")).toHaveCount(4);
  await expect(c.getByRole("checkbox", { name: "Take Address from the other version" })).not.toBeChecked();
  await c.getByRole("checkbox", { name: "Take Address from the other version" }).check();
  await c.getByRole("checkbox", { name: "Take Group from the other version" }).check();
  await expect(apply).toBeEnabled();
  await apply.click();

  await expect(c).toHaveCount(0);
  const [resolution] = (await resolved(page)).map((r) => r.resolution);
  expect(resolution).toHaveProperty("fields");
  expect([...(resolution as { fields: string[] }).fields].sort()).toEqual(["address", "group_id"]);
  expect((await resolved(page))[0].id).toBe(id);
});

test("pick_fields_can_be_cancelled", async ({ page }) => {
  await createVault(page);
  await addConflicts(page, hostConflict());
  await openConflicts(page);
  const c = card(page, "Host · prod-db");
  await c.getByRole("button", { name: "Pick fields" }).click();
  await c.getByRole("button", { name: "Cancel" }).click();
  await expect(c.getByRole("checkbox")).toHaveCount(0);
  await expect(c.getByRole("button", { name: "Keep current" })).toBeVisible();
  expect(await resolved(page)).toEqual([]);
});

test("reveal_secret_in_conflict", async ({ page }) => {
  await createVault(page);
  await page.clock.install();
  await addConflicts(page, {
    kind: "identity",
    current: { id: "i1", label: "deploy", username: "root", password: "hunter2-current" },
    other: { id: "i1", label: "deploy", username: "root", password: "swordfish-other" },
  });
  await openConflicts(page);

  const c = card(page, "Identity · deploy");
  const row = c.getByRole("row", { name: /Password/ });
  // Masked: neither value is anywhere in the page.
  await expect(row).toContainText("••••");
  const html = () => page.evaluate(() => document.body.innerHTML);
  expect(await html()).not.toContain("hunter2-current");
  expect(await html()).not.toContain("swordfish-other");

  // Revealing one side shows only that side.
  await row.getByRole("button", { name: "Reveal Password of the other version" }).click();
  await expect(row).toContainText("swordfish-other");
  expect(await html()).not.toContain("hunter2-current");

  // Hide masks it again, and the value is gone from the page.
  await row.getByRole("button", { name: "Hide Password of the other version" }).click();
  await expect(row).not.toContainText("swordfish-other");
  expect(await html()).not.toContain("swordfish-other");

  // It also re-masks by itself.
  await row.getByRole("button", { name: "Reveal Password of the current version" }).click();
  await expect(row).toContainText("hunter2-current");
  await page.clock.runFor(31_000);
  await expect(row).not.toContainText("hunter2-current");
  expect(await html()).not.toContain("hunter2-current");
  await expect(row.getByRole("button", { name: "Reveal Password of the current version" })).toBeVisible();
});

test("edit_vs_delete_shows_deleted_side", async ({ page }) => {
  await createVault(page);
  const detectedAt = Math.floor(new Date("2026-10-20T12:34:00Z").getTime() / 1000);
  const [id] = await addConflicts(page, {
    kind: "ssh_key",
    current: { id: "k1", label: "laptop key", algorithm: "ssh-ed25519", private_openssh: "KEY", passphrase: null },
    other: null,
    detectedAt,
  });
  await openConflicts(page);

  const c = card(page, "SSH key · laptop key");
  const deleted = c.getByTestId("deleted-side");
  await expect(deleted).toContainText("Deleted on another device");
  // Formatted by the browser, as the app does: Node's locale can differ from Chromium's.
  const day = await page.evaluate((s) => new Date(s * 1000).toLocaleDateString(), detectedAt);
  await expect(deleted).toContainText(day);
  // The edited record stays visible next to it, and nothing in the other column is a value.
  await expect(c.getByRole("row", { name: /Label/ })).toContainText("laptop key");
  // Nothing to pick from a deletion; "Use other version" deletes the record again.
  await expect(c.getByRole("button", { name: "Pick fields" })).toHaveCount(0);
  await expect(c).toContainText("Deletes the record");
  await c.getByRole("button", { name: "Use other version" }).click();
  // The key goes with the record: asks first.
  await page.getByRole("dialog", { name: "Discard the current version's private key?" }).getByRole("button", { name: "Discard" }).click();
  await expect(c).toHaveCount(0);
  expect(await resolved(page)).toEqual([{ id, resolution: { useOther: true } }]);
});

test("resolve_failure_is_shown_and_keeps_the_conflict", async ({ page }) => {
  await createVault(page);
  await addConflicts(page, hostConflict());
  await openConflicts(page);
  // Another window already settled it: the mock no longer knows the id.
  await page.evaluate(async () => {
    const [c] = await window.__tetherMock.conflicts.list();
    await window.__tetherMock.conflicts.resolve(c.id, { keepCurrent: true });
  });
  const c = card(page, "Host · prod-db");
  await c.getByRole("button", { name: "Keep current" }).click();
  await expect(c.getByRole("alert")).toBeVisible();
});

test("locking_drops_conflicts_and_revealed_secrets", async ({ page }) => {
  await createVault(page);
  await addConflicts(page, {
    kind: "identity",
    current: { id: "i1", label: "deploy", username: "root", password: "hunter2-current" },
    other: { id: "i1", label: "deploy", username: "root", password: "swordfish-other" },
  });
  await openConflicts(page);
  await card(page, "Identity · deploy").getByRole("button", { name: "Reveal Password of the other version" }).click();
  await expect(page.getByText("swordfish-other")).toBeVisible();

  await page.getByRole("button", { name: "Lock vault" }).click();
  await expect(page.getByRole("heading", { name: "Unlock" })).toBeVisible();
  // Nothing of it stays in the page while locked.
  expect(await page.evaluate(() => document.body.innerHTML)).not.toContain("swordfish-other");

  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  // Back: the badge counts again; the secret is masked again.
  await expect(badge(page)).toHaveText("1 conflict");
  await expect(page.getByText("swordfish-other")).toHaveCount(0);
});

test("discard_hint_is_shown", async ({ page }) => {
  await createVault(page);
  await addConflicts(page, hostConflict());
  await openConflicts(page);
  const c = card(page, "Host · prod-db");
  await expect(c).toContainText("The version you don't choose is discarded.");
  // Nothing secret: no extra warning and no confirm.
  await expect(c).not.toContainText("secret");
  await c.getByRole("button", { name: "Keep current" }).click();
  await expect(c).toHaveCount(0);
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("keep_current_with_secret_asks_to_confirm", async ({ page }) => {
  await createVault(page);
  const [id] = await addConflicts(page, {
    kind: "identity",
    current: { id: "i1", label: "deploy", username: "root", password: "hunter2-current" },
    other: { id: "i1", label: "deploy", username: "root", password: "swordfish-other" },
  });
  await openConflicts(page);
  const c = card(page, "Identity · deploy");
  await expect(c).toContainText("secret");

  // Keep current drops the other version's password: asks, and Cancel changes nothing.
  await c.getByRole("button", { name: "Keep current" }).click();
  const dialog = page.getByRole("dialog", { name: "Discard the other version's password?" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toBeHidden();
  expect(await resolved(page)).toEqual([]);
  await expect(c).toBeVisible();

  // Use other version drops the current one's.
  await c.getByRole("button", { name: "Use other version" }).click();
  const current = page.getByRole("dialog", { name: "Discard the current version's password?" });
  await expect(current).toBeVisible();
  await current.getByRole("button", { name: "Cancel" }).click();
  expect(await resolved(page)).toEqual([]);

  await c.getByRole("button", { name: "Keep current" }).click();
  await dialog.getByRole("button", { name: "Discard" }).click();
  await expect(c).toHaveCount(0);
  expect(await resolved(page)).toEqual([{ id, resolution: { keepCurrent: true } }]);
});

test("pick_fields_asks_only_for_secrets_left_behind", async ({ page }) => {
  await createVault(page);
  await addConflicts(page, {
    kind: "identity",
    current: { id: "i1", label: "deploy", username: "root", password: "a" },
    other: { id: "i1", label: "deploy", username: "admin", password: "b" },
  });
  await openConflicts(page);
  const c = card(page, "Identity · deploy");
  await c.getByRole("button", { name: "Pick fields" }).click();
  await c.getByRole("checkbox", { name: "Take Username from the other version" }).check();
  await c.getByRole("button", { name: "Apply" }).click();
  // The other password is not taken: it is discarded.
  const dialog = page.getByRole("dialog", { name: "Discard the other version's password?" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await c.getByRole("checkbox", { name: "Take Password from the other version" }).check();
  await c.getByRole("button", { name: "Apply" }).click();
  // Taking the other password replaces the current one, which is then gone: that is asked too.
  const replace = page.getByRole("dialog", { name: "Replace the current version's password?" });
  await expect(replace).toBeVisible();
  await expect(replace).toContainText("replaces the current version's password");
  await replace.getByRole("button", { name: "Cancel" }).click();
  await expect(replace).toBeHidden();
  expect(await resolved(page)).toEqual([]);
  await c.getByRole("button", { name: "Apply" }).click();
  await replace.getByRole("button", { name: "Replace" }).click();
  await expect(c).toHaveCount(0);
  expect((await resolved(page))[0].resolution).toEqual({ fields: ["password", "username"] });
});

test("pick_fields_names_every_secret_it_loses", async ({ page }) => {
  await createVault(page);
  await addConflicts(page, {
    kind: "ssh_key",
    current: { id: "k1", label: "deploy key", private_openssh: "AAA", passphrase: "p1", public_openssh: "ssh-ed25519 A" },
    other: { id: "k1", label: "deploy key", private_openssh: "BBB", passphrase: "p2", public_openssh: "ssh-ed25519 A" },
  });
  await openConflicts(page);
  const c = card(page, "SSH key · deploy key");
  await c.getByRole("button", { name: "Pick fields" }).click();
  await c.getByRole("checkbox", { name: "Take Private key from the other version" }).check();
  await c.getByRole("button", { name: "Apply" }).click();
  // The current private key is replaced and the other passphrase is left behind: both are named.
  const dialog = page.getByRole("dialog", { name: "Lose the current version's private key and the other version's passphrase?" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Discard" }).click();
  await expect(c).toHaveCount(0);
  expect((await resolved(page))[0].resolution).toEqual({ fields: ["private_openssh"] });
});
