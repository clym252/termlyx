import type { Page } from "@playwright/test";
import { expect, test } from "./fixtures";
import { createVault, editHost, folders, goTo, hostForm, hostList, hostRow, identityList, rowOf } from "./helpers";

// Sync pulled changes from another device while a page is open: the mock plays the engine with
// `control.remoteUpsert` / `remoteDelete`, which emit `vault-changed`. Nothing here navigates.

/** The label text of a host in the list, matched exactly. */
const labelOf = (page: Page, label: string) => hostList(page).getByText(label, { exact: true });

const seed = (page: Page, label: string, address: string) =>
  page.evaluate(
    async ({ label, address }) => (await window.__tetherMock.upsertEntity("host", { label, address })).id,
    { label, address },
  );

/** Shows the hosts page with the host list read after `seed` ran. */
async function hostsPageWith(page: Page, hosts: [string, string][]): Promise<string[]> {
  await createVault(page);
  const ids: string[] = [];
  for (const [label, address] of hosts) ids.push(await seed(page, label, address));
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  for (const [label] of hosts) await expect(hostRow(page, label)).toBeVisible();
  return ids;
}

test("remote_host_add_appears_without_navigating", async ({ page }) => {
  await hostsPageWith(page, [["web", "10.0.0.1"]]);
  await expect(hostRow(page, "from-laptop")).toHaveCount(0);

  await page.evaluate(() =>
    window.__tetherMock.control.remoteUpsert("host", { label: "from-laptop", address: "10.0.0.9" }),
  );
  await expect(hostRow(page, "from-laptop")).toBeVisible();
  await expect(hostRow(page, "web")).toBeVisible();
});

test("remote_label_edit_and_delete_update_the_open_list", async ({ page }) => {
  const [web] = await hostsPageWith(page, [
    ["web", "10.0.0.1"],
    ["db", "10.0.0.2"],
  ]);

  await page.evaluate(
    (id) => window.__tetherMock.control.remoteUpsert("host", { id, label: "web-renamed", address: "10.0.0.1" }),
    web,
  );
  await expect(hostRow(page, "web-renamed")).toBeVisible();
  await expect(labelOf(page, "web")).toHaveCount(0);

  await page.evaluate((id) => window.__tetherMock.control.remoteDelete("host", id), web);
  await expect(labelOf(page, "web-renamed")).toHaveCount(0);
  await expect(hostRow(page, "db")).toBeVisible();
});

test("open_host_editor_keeps_typed_text_across_remote_changes", async ({ page }) => {
  const [web] = await hostsPageWith(page, [
    ["web", "10.0.0.1"],
    ["db", "10.0.0.2"],
  ]);
  await editHost(page, "web");
  const form = hostForm(page);
  await form.getByLabel("Label").fill("web (typing…)");
  await form.getByLabel("Address").fill("10.9.9.9");
  await form.getByLabel("New tag").fill("half-typed");

  // Another host appears, then this very host is edited elsewhere: the list follows, the form does not.
  await page.evaluate(() => window.__tetherMock.control.remoteUpsert("host", { label: "extra", address: "1.1.1.1" }));
  await expect(hostRow(page, "extra")).toBeVisible();
  await page.evaluate(
    (id) => window.__tetherMock.control.remoteUpsert("host", { id, label: "web-elsewhere", address: "10.0.0.1" }),
    web,
  );
  await expect(hostRow(page, "web-elsewhere")).toBeVisible();

  await expect(form.getByLabel("Label")).toHaveValue("web (typing…)");
  await expect(form.getByLabel("Address")).toHaveValue("10.9.9.9");
  await expect(form.getByLabel("New tag")).toHaveValue("half-typed");

  // Saving then writes what was typed.
  await form.getByRole("button", { name: "Save host" }).click();
  await expect(hostRow(page, "web (typing…)")).toBeVisible();
});

test("open_host_editor_survives_a_remote_delete", async ({ page }) => {
  const [web] = await hostsPageWith(page, [["web", "10.0.0.1"]]);
  await editHost(page, "web");
  const form = hostForm(page);
  await form.getByLabel("Label").fill("web, still typing");

  await page.evaluate((id) => window.__tetherMock.control.remoteDelete("host", id), web);
  await expect(labelOf(page, "web")).toHaveCount(0);

  // The editor stays, with the typed text; saving writes the host again.
  await expect(form).toBeVisible();
  await expect(form.getByLabel("Label")).toHaveValue("web, still typing");
  await form.getByRole("button", { name: "Save host" }).click();
  await expect(hostRow(page, "web, still typing")).toBeVisible();
});

test("open_group_editor_keeps_typed_text_across_a_remote_change", async ({ page }) => {
  await createVault(page);
  const groupId = await page.evaluate(async () => (await window.__tetherMock.upsertEntity("group", { name: "Prod" })).id);
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  const groups = folders(page);
  await expect(groups.getByRole("button", { name: "Prod", exact: true })).toBeVisible();

  await groups.getByRole("button", { name: "Actions for Prod" }).click();
  await page.getByRole("menu", { name: "Prod" }).getByRole("menuitem", { name: "Edit group" }).click();
  const dialog = page.getByRole("dialog", { name: "Edit group Prod" });
  await dialog.getByLabel("Name").fill("Production (typing…)");

  await page.evaluate(() => window.__tetherMock.control.remoteUpsert("group", { name: "Staging" }));
  await expect(groups.getByRole("button", { name: "Staging", exact: true })).toBeVisible();
  await expect(dialog.getByLabel("Name")).toHaveValue("Production (typing…)");

  // The group is deleted elsewhere: the dialog stays open with its text.
  await page.evaluate((id) => window.__tetherMock.control.remoteDelete("group", id), groupId);
  await expect(groups.getByRole("button", { name: "Prod", exact: true })).toHaveCount(0);
  await expect(dialog.getByLabel("Name")).toHaveValue("Production (typing…)");
});

test("keychain_follows_remote_identities_and_keeps_an_open_editor", async ({ page }) => {
  await createVault(page);
  const identityId = await page.evaluate(
    async () => (await window.__tetherMock.upsertEntity("identity", { label: "ops", username: "root" })).id,
  );
  await goTo(page, "Keychain");
  await expect(rowOf(identityList(page), "ops")).toBeVisible();

  await page.evaluate(() =>
    window.__tetherMock.control.remoteUpsert("identity", { label: "from-laptop", username: "deploy" }),
  );
  await expect(rowOf(identityList(page), "from-laptop")).toBeVisible();

  await page.getByRole("button", { name: "Edit identity ops" }).click();
  const dialog = page.getByRole("dialog", { name: "Edit identity ops" });
  await dialog.getByLabel("Username").fill("typing-a-name");
  await page.evaluate(
    (id) => window.__tetherMock.control.remoteUpsert("identity", { id, label: "ops2", username: "root" }),
    identityId,
  );
  await expect(rowOf(identityList(page), "ops2")).toBeVisible();
  await expect(dialog.getByLabel("Username")).toHaveValue("typing-a-name");
  await page.evaluate((id) => window.__tetherMock.control.remoteDelete("identity", id), identityId);
  await expect(rowOf(identityList(page), "ops2")).toHaveCount(0);
  await expect(dialog.getByLabel("Username")).toHaveValue("typing-a-name");
});

test("forwarding_and_known_hosts_pages_follow_remote_changes", async ({ page }) => {
  await createVault(page);
  const hostId = await seed(page, "web", "10.0.0.1");
  await goTo(page, "Port Forwarding");
  await expect(page.getByText("No forwarding rules yet")).toBeVisible();
  await page.evaluate(
    (hostId) =>
      window.__tetherMock.control.remoteUpsert("forward_rule", {
        hostId,
        label: "db tunnel",
        kind: "local",
        bindAddr: "127.0.0.1",
        bindPort: 15432,
        destHost: "db.internal",
        destPort: 5432,
        autostart: false,
      }),
    hostId,
  );
  await expect(page.getByRole("list", { name: "Forwarding rules" }).getByText("db tunnel")).toBeVisible();

  await goTo(page, "Known Hosts");
  await expect(page.getByText("No known hosts yet")).toBeVisible();
  await page.evaluate(() =>
    window.__tetherMock.control.remoteUpsert("known_host", {
      hostPattern: "pulled.example",
      port: 22,
      algorithm: "ssh-ed25519",
      publicKey: "AAAAC3NzaC1lZDI1NTE5AAAAIPulled",
    }),
  );
  await expect(page.getByRole("table", { name: "Known hosts" }).getByText("pulled.example")).toBeVisible();
});
