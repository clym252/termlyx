import { expect, test } from "./fixtures";
import type { Page } from "@playwright/test";
import {
  PASSWORD,
  breadcrumb,
  createGroup,
  createHost,
  createVault,
  editHost,
  folders,
  hostCard,
  hostForm,
  hostList,
  hostRow,
  heldValues,
  holdFields,
  hostsBar,
  nav,
  goTo,
  identityList,
  openGroupPath,
  seedHost,
  sessionIds,
  terminalTabs,
} from "./helpers";

/** The breadcrumb segment of the group being shown ("All hosts" at the top level). */
const here = (page: Page) => breadcrumb(page).locator('[aria-current="location"]');
const filterBox = (page: Page) => page.getByRole("searchbox", { name: "Filter hosts" });

/** Opens a menu item of the current group's "⋯" (after the last breadcrumb segment). */
async function currentGroupAction(page: Page, group: string, item: string) {
  await breadcrumb(page).getByRole("button", { name: `Actions for ${group}` }).click();
  await page.getByRole("menu", { name: group }).getByRole("menuitem", { name: item }).click();
}

/** Opens a menu item of a folder card's "⋯". */
async function folderAction(page: Page, group: string, item: string) {
  await folders(page).getByRole("button", { name: `Actions for ${group}` }).click();
  await page.getByRole("menu", { name: group }).getByRole("menuitem", { name: item }).click();
}

test("create_vault_shows_recovery_once", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Create your vault" })).toBeVisible();

  // Validation: too short, then mismatched.
  const password = page.getByLabel("Master password", { exact: true });
  const confirm = page.getByLabel("Confirm master password");
  await password.fill("short");
  await confirm.fill("short");
  await page.getByRole("button", { name: "Create vault" }).click();
  await expect(page.getByRole("alert")).toContainText("at least 8 characters");
  await password.fill(PASSWORD);
  await confirm.fill(`${PASSWORD}x`);
  await page.getByRole("button", { name: "Create vault" }).click();
  await expect(page.getByRole("alert")).toContainText("do not match");
  await expect(page.getByRole("dialog")).toHaveCount(0);

  await confirm.fill(PASSWORD);
  await page.getByRole("button", { name: "Create vault" }).click();

  // The recovery key is shown in a modal that only "I saved it" dismisses.
  const dialog = page.getByRole("dialog", { name: "Your recovery key" });
  await expect(dialog).toBeVisible();
  const key = await dialog.getByLabel("Recovery key", { exact: true }).inputValue();
  expect(key).toMatch(/^MOCK-/);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeVisible();

  await dialog.getByRole("button", { name: "Copy recovery key" }).click();
  await expect(dialog.getByRole("status")).toHaveText("Copied");
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(key);

  await dialog.getByRole("button", { name: "I saved it" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();

  // Never shown again: not in the page, not after locking and unlocking.
  expect(await page.content()).not.toContain(key);
  await page.getByRole("button", { name: "Lock vault" }).click();
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(await page.content()).not.toContain(key);
});

test("unlock_wrong_password_shows_error", async ({ page }) => {
  const recoveryKey = await createVault(page);
  await page.getByRole("button", { name: "Lock vault" }).click();
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();

  const password = page.getByLabel("Master password");
  await password.fill("not the password");
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("Wrong master password");
  // The submitted password is not kept in the form.
  await expect(password).toHaveValue("");
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();

  // Recovery path: a wrong key is refused, the right one sets a new password.
  await page.getByRole("button", { name: "Use recovery key" }).click();
  const recovery = page.getByLabel("Recovery key", { exact: true });
  await recovery.fill("MOCK-wrong");
  await page.getByLabel("New master password", { exact: true }).fill("a brand new password");
  await page.getByLabel("Confirm new master password").fill("a brand new password");
  await page.getByRole("button", { name: "Reset password and unlock" }).click();
  await expect(page.getByRole("alert")).toContainText("recovery key is wrong");
  await expect(recovery).toHaveValue("");

  await recovery.fill(recoveryKey);
  await page.getByLabel("New master password", { exact: true }).fill("a brand new password");
  await page.getByLabel("Confirm new master password").fill("a brand new password");
  await page.getByRole("button", { name: "Reset password and unlock" }).click();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();

  // The new password works from now on; the old one does not.
  await page.getByRole("button", { name: "Lock vault" }).click();
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("Wrong master password");
  await page.getByLabel("Master password").fill("a brand new password");
  await page.getByRole("button", { name: "Unlock", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
});

test("create_group_tree_and_host", async ({ page }) => {
  await createVault(page);

  // The sidebar lists every section; only Hosts is built so far.
  for (const section of ["Hosts", "Keychain", "Port Forwarding", "Known Hosts", "SFTP", "Settings"]) {
    await expect(nav(page).getByRole("button", { name: section, exact: true })).toBeVisible();
  }

  await createGroup(page, "Production");
  await createGroup(page, "Staging");
  await createGroup(page, "Web", { parent: "Production" });

  // Web is nested under Production, not under Staging, and not at the top.
  await expect(folders(page).getByRole("button", { name: "Production", exact: true })).toBeVisible();
  await expect(folders(page).getByRole("button", { name: "Staging", exact: true })).toBeVisible();
  await expect(folders(page).getByRole("button", { name: "Web", exact: true })).toHaveCount(0);
  await openGroupPath(page, "Staging");
  await expect(folders(page)).toHaveCount(0);
  await openGroupPath(page, "Production");
  await expect(folders(page).getByRole("button", { name: "Web", exact: true })).toBeVisible();

  // A new host starts in the current group; unicode survives.
  await openGroupPath(page, "Production / Web");
  await hostsBar(page).getByRole("button", { name: "New host" }).click();
  await expect(hostForm(page).getByLabel("Group", { exact: true })).toHaveValue(/.+/);
  await hostForm(page).getByLabel("Label").fill("本番 web-01 🚀");
  await hostForm(page).getByLabel("Address").fill("10.0.0.5");
  await hostForm(page).getByRole("button", { name: "Save host" }).click();
  await expect(hostRow(page, "本番 web-01 🚀")).toBeVisible();

  // Only its own group lists it; from an ancestor the filter finds it, with its path.
  await openGroupPath(page, "Staging");
  await expect(page.getByText("This group is empty.")).toBeVisible();
  await openGroupPath(page, "Production");
  await expect(hostRow(page, "本番 web-01 🚀")).toHaveCount(0);
  await filterBox(page).fill("web-01");
  await expect(hostCard(page, "本番 web-01 🚀")).toContainText("Production / Web");
  await page.getByRole("button", { name: "Clear filter" }).click();
  await openGroupPath(page, "Production / Web");
  await editHost(page, "本番 web-01 🚀");
  await expect(hostForm(page).getByLabel("Label")).toHaveValue("本番 web-01 🚀");
  await expect(
    hostForm(page).getByLabel("Group", { exact: true }).locator("option:checked"),
  ).toHaveText("Production / Web");

  // Rename a group.
  await openGroupPath(page, "");
  await folderAction(page, "Staging", "Edit group");
  const dialog = page.getByRole("dialog", { name: "Edit group Staging" });
  await dialog.getByLabel("Name").fill("Pré-prod ✓");
  await dialog.getByRole("button", { name: "Save group" }).click();
  await expect(folders(page).getByRole("button", { name: "Pré-prod ✓", exact: true })).toBeVisible();
});

test("host_inherits_port_from_group_with_source_label", async ({ page }) => {
  await createVault(page);
  await createGroup(page, "Production", { port: 2200 });
  await createGroup(page, "Web", { parent: "Production" });

  // A host outside any group: the app default.
  await openGroupPath(page, "");
  await hostsBar(page).getByRole("button", { name: "New host" }).click();
  let form = hostForm(page);
  // Inheriting: empty, the inherited value as its placeholder, still editable.
  await expect(form.getByLabel("Port", { exact: true })).toHaveValue("");
  await expect(form.getByLabel("Port", { exact: true })).toHaveAttribute("placeholder", "22");
  await expect(form.getByLabel("Port", { exact: true })).toBeEnabled();
  await expect(form.getByLabel("Port", { exact: true })).toHaveAccessibleDescription(/default/);

  // An unsaved host picks up the chain of the group chosen in the form.
  await form.getByLabel("Label").fill("web-01");
  await form.getByLabel("Address").fill("10.0.0.5");
  await form.getByLabel("Group", { exact: true }).selectOption({ label: "Production / Web" });
  await expect(form.getByLabel("Port", { exact: true })).toHaveAttribute("placeholder", "2200");
  await expect(form.getByLabel("Port", { exact: true })).toHaveAccessibleDescription(
    /from group Production/,
  );
  await expect(form.getByText("from group Production").first()).toBeVisible();
  await form.getByRole("button", { name: "Save host" }).click();
  await openGroupPath(page, "Production / Web");
  await expect(hostRow(page, "web-01")).toBeVisible();

  // The saved host reports the same source (from the backend this time).
  await expect(form).toBeHidden();
  await editHost(page, "web-01");
  form = hostForm(page);
  await expect(form.getByLabel("Port", { exact: true })).toHaveValue("");
  await expect(form.getByLabel("Port", { exact: true })).toHaveAttribute("placeholder", "2200");
  await expect(form.getByLabel("Port", { exact: true })).toHaveAccessibleDescription(
    /from group Production/,
  );

  // Typing a value gives the host its own; the source becomes the host.
  await form.getByLabel("Port", { exact: true }).fill("2022");
  await expect(form.getByLabel("Port", { exact: true })).toHaveAccessibleDescription(/this host/);
  await form.getByRole("button", { name: "Save host" }).click();
  await expect(form).toBeHidden();
  await editHost(page, "web-01");
  form = hostForm(page);
  await expect(form.getByLabel("Port", { exact: true })).toHaveValue("2022");

  // Clearing it goes back to inheriting.
  await form.getByLabel("Port", { exact: true }).fill("");
  await expect(form.getByLabel("Port", { exact: true })).toHaveAttribute("placeholder", "2200");
  await expect(form.getByLabel("Port", { exact: true })).toHaveAccessibleDescription(
    /from group Production/,
  );
  await form.getByRole("button", { name: "Save host" }).click();
  await expect(form).toBeHidden();
  await editHost(page, "web-01");
  form = hostForm(page);
  await expect(form.getByLabel("Port", { exact: true })).toHaveValue("");
  await expect(form.getByLabel("Port", { exact: true })).toHaveAttribute("placeholder", "2200");

  // A group shows what it inherits too.
  await currentGroupAction(page, "Web", "Edit group");
  const dialog = page.getByRole("dialog", { name: "Edit group Web" });
  await expect(dialog.getByLabel("Port", { exact: true })).toHaveAttribute("placeholder", "2200");
  await expect(dialog.getByLabel("Port", { exact: true })).toHaveAccessibleDescription(
    /from group Production/,
  );
});

test("delete_group_asks_child_policy", async ({ page }) => {
  await createVault(page);
  await createGroup(page, "Team");
  await createGroup(page, "Sub", { parent: "Team" });
  await createHost(page, { label: "in-team", address: "10.1.0.1", group: "Team" });
  await createHost(page, { label: "in-sub", address: "10.1.0.2", group: "Team / Sub" });

  // Team has children: the dialog asks what happens to them.
  await folderAction(page, "Team", "Delete group");
  let dialog = page.getByRole("dialog", { name: "Delete group Team" });
  await expect(dialog).toContainText("1 subgroup");
  await expect(dialog).toContainText("2 hosts");
  const move = dialog.getByRole("radio", { name: "Move contents to the parent group" });
  const wipe = dialog.getByRole("radio", { name: "Delete the group and everything in it" });
  await expect(move).toBeChecked();
  await expect(wipe).not.toBeChecked();

  // Cancel leaves everything alone.
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(dialog).toBeHidden();
  await expect(folders(page).getByRole("button", { name: "Team", exact: true })).toBeVisible();

  // Move up: Team is gone, Sub is top level, both hosts survive (in-team now ungrouped).
  await folderAction(page, "Team", "Delete group");
  dialog = page.getByRole("dialog", { name: "Delete group Team" });
  await dialog.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(folders(page).getByRole("button", { name: "Team", exact: true })).toHaveCount(0);
  await expect(folders(page).getByRole("button", { name: "Sub", exact: true })).toBeVisible();
  await expect(hostRow(page, "in-team")).toBeVisible();
  await openGroupPath(page, "Sub");
  await expect(hostRow(page, "in-sub")).toBeVisible();

  // Delete everything: Sub and its host go; the host that moved up stays.
  await currentGroupAction(page, "Sub", "Delete group");
  dialog = page.getByRole("dialog", { name: "Delete group Sub" });
  await expect(dialog).toContainText("1 host");
  await dialog.getByRole("radio", { name: "Delete the group and everything in it" }).check();
  await dialog.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(here(page)).toHaveText("All hosts");
  await expect(folders(page)).toHaveCount(0);
  await filterBox(page).fill("in-");
  await expect(hostRow(page, "in-sub")).toHaveCount(0);
  await expect(hostRow(page, "in-team")).toBeVisible();
  await page.getByRole("button", { name: "Clear filter" }).click();

  // An empty group is a plain confirmation without a policy choice.
  await createGroup(page, "Empty");
  await openGroupPath(page, "Empty");
  await currentGroupAction(page, "Empty", "Delete group");
  dialog = page.getByRole("dialog", { name: "Delete group Empty" });
  await expect(dialog.getByRole("radio")).toHaveCount(0);
  await dialog.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(here(page)).toHaveText("All hosts");
  await expect(folders(page)).toHaveCount(0);
});

test("tag_filter_and_search", async ({ page }) => {
  await createVault(page);
  await createHost(page, { label: "db-primary", address: "10.0.1.10", tags: ["prod", "db"] });
  await createHost(page, { label: "web-front", address: "10.0.2.20", tags: ["prod"] });
  await createHost(page, { label: "stage-box", address: "10.0.3.30", tags: ["staging"] });
  await createHost(page, { label: "東京 ノード", address: "192.168.9.9", tags: ["asia"] });
  await expect(hostList(page).getByRole("listitem")).toHaveCount(4);

  // Tag filter: a host must carry every selected tag.
  const filter = page.getByRole("group", { name: "Filter by tag" });
  await filter.getByRole("button", { name: "prod", exact: true }).click();
  await expect(filter.getByRole("button", { name: "prod", exact: true })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(hostRow(page, "db-primary")).toBeVisible();
  await expect(hostRow(page, "web-front")).toBeVisible();
  await expect(hostRow(page, "stage-box")).toHaveCount(0);
  await expect(hostRow(page, "東京 ノード")).toHaveCount(0);
  await filter.getByRole("button", { name: "db", exact: true }).click();
  await expect(hostList(page).getByRole("listitem")).toHaveCount(1);
  await expect(hostRow(page, "db-primary")).toBeVisible();
  await filter.getByRole("button", { name: "Clear", exact: true }).click();
  await expect(hostList(page).getByRole("listitem")).toHaveCount(4);

  // The text filter: label, address or username; nothing found says so.
  await filterBox(page).fill("192.168");
  await expect(hostList(page).getByRole("listitem")).toHaveCount(1);
  await expect(hostRow(page, "東京 ノード")).toBeVisible();
  await filterBox(page).fill("nothing-matches-this");
  await expect(page.getByText("No hosts match the filter.")).toBeVisible();
  await page.getByRole("button", { name: "Clear filter" }).click();
  await expect(filterBox(page)).toHaveValue("");
  await expect(hostList(page).getByRole("listitem")).toHaveCount(4);

  // Global search (Cmd+K on the pinned macOS platform): label, address and tag name.
  await page.keyboard.press("Meta+k");
  const dialog = page.getByRole("dialog", { name: "Search" });
  await expect(dialog).toBeVisible();
  const box = dialog.getByRole("combobox", { name: "Search" });
  await expect(box).toBeFocused();
  const results = dialog.getByRole("option");

  await box.fill("front");
  await expect(results).toHaveCount(1);
  await expect(results.first()).toContainText("web-front");

  await box.fill("10.0.1");
  await expect(results).toHaveCount(1);
  await expect(results.first()).toContainText("db-primary");

  await box.fill("PROD");
  await expect(results).toHaveCount(2);

  await box.fill("staging");
  await expect(results).toHaveCount(1);
  await expect(results.first()).toContainText("stage-box");

  await box.fill("nothing-matches-this");
  await expect(results).toHaveCount(0);
  await expect(dialog).toContainText("Nothing matches");

  // Alt+Enter opens the host's details (Enter connects).
  await box.fill("東京");
  await expect(results).toHaveCount(1);
  await box.press("Alt+Enter");
  await expect(dialog).toBeHidden();
  await expect(hostForm(page).getByLabel("Label")).toHaveValue("東京 ノード");

  // It works from another section too, and Escape closes it.
  await nav(page).getByRole("button", { name: "Settings", exact: true }).click();
  await page.keyboard.press("Meta+k");
  await expect(dialog).toBeVisible();
  await box.fill("db-pri");
  await box.press("Alt+Enter");
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeVisible();
  await expect(hostForm(page).getByLabel("Label")).toHaveValue("db-primary");
  await page.keyboard.press("Meta+k");
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});

test("search_pick_goes_to_the_hosts_group_and_clears_the_filter", async ({ page }) => {
  await createVault(page);
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    const lab = await api.upsertEntity("group", { name: "Lab" });
    const rack = await api.upsertEntity("group", { name: "Rack", parentId: lab.id });
    await api.upsertEntity("host", { label: "pve", address: "10.0.0.2", groupId: rack.id });
    await api.upsertEntity("host", { label: "nas", address: "10.0.0.5" });
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  await filterBox(page).fill("nas");
  await expect(hostList(page).getByRole("listitem")).toHaveCount(1);

  await page.keyboard.press("Meta+k");
  const box = page.getByRole("dialog", { name: "Search" }).getByRole("combobox", { name: "Search" });
  await box.fill("pve");
  await box.press("Alt+Enter");
  await expect(here(page)).toHaveText("Rack");
  await expect(breadcrumb(page)).toContainText("Lab");
  await expect(filterBox(page)).toHaveValue("");
  await expect(hostRow(page, "pve")).toBeVisible();
  await expect(hostCard(page, "pve").locator(".host-card")).toHaveAttribute("data-editing", "true");
  await expect(hostForm(page).getByLabel("Label")).toHaveValue("pve");
});

test("own_login_on_host_and_group", async ({ page }) => {
  await createVault(page);
  const ids = await page.evaluate(async () => {
    const api = window.__tetherMock;
    const keychain = await api.upsertEntity("identity", { label: "deploy key", username: "deploy" });
    const group = await api.upsertEntity("group", { name: "Production" });
    const host = await api.upsertEntity("host", { label: "web1", address: "10.0.0.11", groupId: group.id });
    return { keychain: keychain.id, group: group.id, host: host.id };
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  const identities = () =>
    page.evaluate(async () =>
      (await window.__tetherMock.listEntities("identity")).map((i) => ({
        owner: i.owner,
        username: i.username,
        hasPassword: i.hasPassword,
      })),
    );

  // The group gets its own username and password.
  await folderAction(page, "Production", "Edit group");
  const dialog = page.getByRole("dialog", { name: "Edit group Production" });
  await dialog.getByLabel("Identity", { exact: true }).selectOption({ label: "Own username & password" });
  await dialog.getByLabel("Username", { exact: true }).fill("alice");
  await dialog.getByLabel("Password", { exact: true }).fill("lab-pw");
  await dialog.getByRole("button", { name: "Save group" }).click();
  await expect(dialog).toBeHidden();
  expect(await identities()).toContainEqual({ owner: ids.group, username: "alice", hasPassword: true });

  // The host inherits it, and says so (its card too).
  await openGroupPath(page, "Production");
  await expect(hostCard(page, "web1")).toContainText("alice@10.0.0.11");
  await editHost(page, "web1");
  const form = hostForm(page);
  await expect(form.getByLabel("Identity", { exact: true }).locator("option:checked")).toHaveText(
    "Same as group Production (Production login (alice))",
  );
  await expect(form).toContainText("from group Production");
  // Own logins are not offered as Keychain identities.
  const options = await form.getByLabel("Identity", { exact: true }).locator("option").allInnerTexts();
  expect(options).toContain("deploy key (deploy)");

  // Picking own login under an inherited own login starts the host's own (prefilled), never links the group's.
  await form.getByLabel("Identity", { exact: true }).selectOption({ label: "Own username & password" });
  await expect(form.getByLabel("Identity", { exact: true })).toHaveValue("own");
  await expect(form.getByLabel("Username", { exact: true })).toHaveValue("alice");
  // The host gets its own login, first without a password.
  await form.getByLabel("Username", { exact: true }).fill("root");
  await form.getByRole("button", { name: "Save host" }).click();
  await expect.poll(identities).toContainEqual({ owner: ids.host, username: "root", hasPassword: false });
  // The editor closes; reopened, it shows the own login. A password is added; later a blank field keeps it.
  await expect(form).toBeHidden();
  await editHost(page, "web1");
  await expect(form.getByLabel("Identity", { exact: true }).locator("option:checked")).toHaveText(
    "Own username & password",
  );
  await expect(form.getByLabel("Username", { exact: true })).toHaveValue("root");
  await form.getByLabel("Password", { exact: true }).fill("root-pw");
  await form.getByRole("button", { name: "Save host" }).click();
  await expect.poll(identities).toContainEqual({ owner: ids.host, username: "root", hasPassword: true });
  await editHost(page, "web1");
  await expect(form.getByLabel("Password", { exact: true })).toHaveAttribute("placeholder", /saved/i);
  await form.getByRole("button", { name: "Save host" }).click();
  await expect.poll(identities).toContainEqual({ owner: ids.host, username: "root", hasPassword: true });
  expect(await page.content()).not.toContain("root-pw");

  // Back to inheriting: the host's own login goes, the group's stays.
  await editHost(page, "web1");
  await form.getByLabel("Identity", { exact: true }).selectOption({ index: 0 });
  await form.getByRole("button", { name: "Save host" }).click();
  await expect.poll(async () => (await identities()).filter((i) => i.owner === ids.host)).toEqual([]);
  expect((await identities()).filter((i) => i.owner === ids.group)).toHaveLength(1);

  // The Keychain lists only Keychain identities.
  await goTo(page, "Keychain");
  const list = identityList(page);
  await expect(list).toContainText("deploy key");
  await expect(list).not.toContainText("Production");
});

test("group_row_menu_and_counts", async ({ page }) => {
  await createVault(page);
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    const prod = await api.upsertEntity("group", { name: "Production" });
    const web = await api.upsertEntity("group", { name: "Web", parentId: prod.id });
    await api.upsertEntity("host", { label: "db1", address: "10.0.0.21", groupId: prod.id });
    await api.upsertEntity("host", { label: "web1", address: "10.0.0.11", groupId: web.id });
    await api.upsertEntity("host", { label: "pi", address: "192.168.1.5" });
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  // Counts: each folder with what is below it.
  await expect(folders(page).getByRole("button", { name: "Production", exact: true })).toContainText("2 hosts");
  await openGroupPath(page, "Production");
  await expect(folders(page).getByRole("button", { name: "Web", exact: true })).toContainText("1 host");

  // The folder's own menu, without opening the group first.
  // "⋯" opens and closes it.
  const more = folders(page).getByRole("button", { name: "Actions for Web" });
  await more.click();
  await expect(page.getByRole("menu", { name: "Web" })).toBeVisible();
  await more.click();
  await expect(page.getByRole("menu", { name: "Web" })).toHaveCount(0);
  await expect(more).toHaveAttribute("aria-expanded", "false");
  await more.click();
  const menu = page.getByRole("menu", { name: "Web" });
  await expect(menu.getByRole("menuitem")).toHaveText(["Open", "New subgroup", "Edit group", "Delete group"]);
  await menu.getByRole("menuitem", { name: "Edit group" }).click();
  await expect(page.getByRole("dialog", { name: "Edit group Web" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();

  // The current group's menu sits after the breadcrumb.
  await breadcrumb(page).getByRole("button", { name: "Actions for Production" }).click();
  await expect(page.getByRole("menu", { name: "Production" }).getByRole("menuitem")).toHaveText([
    "New subgroup",
    "Edit group",
    "Delete group",
  ]);
  await page.keyboard.press("Escape");

  // Right-click opens a folder's menu too; Escape closes it.
  await openGroupPath(page, "");
  await folders(page).getByRole("button", { name: "Production", exact: true }).click({ button: "right" });
  await expect(page.getByRole("menu", { name: "Production" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
  await folders(page).getByRole("button", { name: "Production", exact: true }).click({ button: "right" });
  await page.getByRole("menu", { name: "Production" }).getByRole("menuitem", { name: "New subgroup" }).click();
  await expect(page.getByRole("dialog", { name: "New group" }).getByLabel("Parent group")).toHaveValue(/.+/);
});

test("folder_navigation_and_breadcrumb", async ({ page }) => {
  await createVault(page);
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    const homelab = await api.upsertEntity("group", { name: "Homelab" });
    const proxmox = await api.upsertEntity("group", { name: "Proxmox", parentId: homelab.id });
    await api.upsertEntity("host", { label: "router", address: "10.0.0.1", groupId: homelab.id });
    await api.upsertEntity("host", { label: "pve-1", address: "10.0.0.10", groupId: proxmox.id });
    await api.upsertEntity("host", { label: "laptop", address: "10.0.0.50" });
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");

  // The top level: the one folder, and every host (the grouped ones too).
  await expect(here(page)).toHaveText("All hosts");
  await expect(folders(page).getByRole("listitem")).toHaveCount(1);
  await expect(folders(page).getByRole("button", { name: "Homelab", exact: true })).toBeVisible();
  await expect(hostList(page).getByRole("listitem")).toHaveCount(3);
  await expect(hostRow(page, "laptop")).toBeVisible();
  await expect(hostRow(page, "router")).toBeVisible();
  await expect(hostRow(page, "pve-1")).toBeVisible();

  // Inside Homelab: its subgroup and its own host.
  await folders(page).getByRole("button", { name: "Homelab", exact: true }).click();
  await expect(here(page)).toHaveText("Homelab");
  await expect(folders(page).getByRole("button", { name: "Proxmox", exact: true })).toBeVisible();
  await expect(hostList(page).getByRole("listitem")).toHaveCount(1);
  await expect(hostRow(page, "router")).toBeVisible();

  // One more level, then back up through the breadcrumb.
  await folders(page).getByRole("button", { name: "Proxmox", exact: true }).click();
  await expect(here(page)).toHaveText("Proxmox");
  await expect(folders(page)).toHaveCount(0);
  await expect(hostRow(page, "pve-1")).toBeVisible();
  await breadcrumb(page).getByRole("button", { name: "Homelab", exact: true }).click();
  await expect(here(page)).toHaveText("Homelab");
  await breadcrumb(page).getByRole("button", { name: "All hosts" }).click();
  await expect(here(page)).toHaveText("All hosts");
  await expect(hostRow(page, "laptop")).toBeVisible();
  // At the top already, "All hosts" changes nothing.
  await breadcrumb(page).getByRole("button", { name: "All hosts" }).click();
  await expect(here(page)).toHaveText("All hosts");
  await expect(folders(page).getByRole("listitem")).toHaveCount(1);
});

test("all_hosts_lists_every_host_with_group_path", async ({ page }) => {
  await createVault(page);
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    const homelab = await api.upsertEntity("group", { name: "Homelab" });
    const proxmox = await api.upsertEntity("group", { name: "Proxmox", parentId: homelab.id });
    await api.upsertEntity("host", { label: "router", address: "10.0.0.1", groupId: homelab.id });
    await api.upsertEntity("host", { label: "pve-1", address: "10.0.0.10", groupId: proxmox.id });
    await api.upsertEntity("host", { label: "laptop", address: "10.0.0.50" });
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");

  // All hosts: all three, each grouped one labelled with where it lives.
  await expect(here(page)).toHaveText("All hosts");
  await expect(hostList(page).getByRole("listitem")).toHaveCount(3);
  await expect(hostCard(page, "pve-1").locator(".host-card-path")).toHaveText("Homelab / Proxmox");
  await expect(hostCard(page, "router").locator(".host-card-path")).toHaveText("Homelab");
  await expect(hostCard(page, "laptop").locator(".host-card-path")).toHaveCount(0);

  // Inside a group: only that group's own hosts, no path line.
  await openGroupPath(page, "Homelab");
  await expect(hostList(page).getByRole("listitem")).toHaveCount(1);
  await expect(hostRow(page, "router")).toBeVisible();
  await expect(hostCard(page, "router").locator(".host-card-path")).toHaveCount(0);
});

test("filter_shows_flat_results", async ({ page }) => {
  await createVault(page);
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    const homelab = await api.upsertEntity("group", { name: "Homelab" });
    const proxmox = await api.upsertEntity("group", { name: "Proxmox", parentId: homelab.id });
    await api.upsertEntity("host", { label: "router", address: "10.0.0.1", groupId: homelab.id });
    await api.upsertEntity("host", { label: "prox-1", address: "10.0.0.10", groupId: proxmox.id });
    await api.upsertEntity("host", { label: "prox-laptop", address: "10.0.0.50" });
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  await openGroupPath(page, "Homelab");

  // Inside Homelab the filter searches Homelab's whole subtree (not the ungrouped host).
  await filterBox(page).fill("prox");
  await expect(folders(page)).toHaveCount(0);
  await expect(hostList(page).getByRole("listitem")).toHaveCount(1);
  await expect(hostCard(page, "prox-1")).toContainText("Homelab / Proxmox");

  await page.getByRole("button", { name: "Clear filter" }).click();
  await expect(filterBox(page)).toHaveValue("");
  await expect(folders(page).getByRole("button", { name: "Proxmox", exact: true })).toBeVisible();
  await expect(hostRow(page, "router")).toBeVisible();
  await expect(here(page)).toHaveText("Homelab");
});

test("drag_host_onto_folder_and_breadcrumb", async ({ page }) => {
  await createVault(page);
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    await api.upsertEntity("group", { name: "Lab" });
    const tag = await api.upsertEntity("tag", { name: "pi", color: "#0a0" });
    await api.upsertEntity("host", { label: "pi", address: "192.168.1.5", tagIds: [tag.id] });
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  const groupOf = () =>
    page.evaluate(async () => {
      const [h] = await window.__tetherMock.listEntities("host");
      return { groupId: h.groupId, tags: h.tagIds.length };
    });

  // Onto a folder: into that group, everything else kept.
  await hostRow(page, "pi").dragTo(folders(page).getByRole("button", { name: "Lab", exact: true }));
  await expect.poll(async () => (await groupOf()).groupId).not.toBeNull();
  expect((await groupOf()).tags).toBe(1);
  // Still listed at the top level, now with its group's path.
  await expect(hostCard(page, "pi")).toContainText("Lab");
  await expect(folders(page).getByRole("button", { name: "Lab", exact: true })).toContainText("1 host");

  // Onto "All hosts" in the breadcrumb: out of any group.
  await openGroupPath(page, "Lab");
  await hostRow(page, "pi").dragTo(breadcrumb(page).getByRole("button", { name: "All hosts" }));
  await expect.poll(async () => (await groupOf()).groupId).toBeNull();
  await expect(page.getByText("This group is empty.")).toBeVisible();
  // Back at the top level it is listed without a path line.
  await openGroupPath(page, "");
  await expect(hostRow(page, "pi")).toBeVisible();
  await expect(hostCard(page, "pi").locator(".host-card-path")).toHaveCount(0);
});

test("drop_on_own_group_is_noop", async ({ page }) => {
  await createVault(page);
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    await api.upsertEntity("host", { label: "nas", address: "10.0.0.5" });
    await api.upsertEntity("host", { label: "pi", address: "10.0.0.2" });
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  await page.evaluate(() => {
    const api = window.__tetherMock;
    const upsert = api.upsertEntity.bind(api);
    (window as unknown as { __writes: number }).__writes = 0;
    api.upsertEntity = ((...args: Parameters<typeof upsert>) => {
      (window as unknown as { __writes: number }).__writes++;
      return upsert(...args);
    }) as typeof upsert;
  });

  // Onto "All hosts", where it already is, then onto another host's card.
  await hostRow(page, "nas").dragTo(breadcrumb(page).getByRole("button", { name: "All hosts" }));
  await hostRow(page, "nas").dragTo(hostRow(page, "pi"));
  // Neither drop opened a session or the editor.
  await expect(hostForm(page)).toHaveCount(0);
  expect(await sessionIds(page)).toEqual([]);
  expect(await page.evaluate(() => (window as unknown as { __writes: number }).__writes)).toBe(0);
});

test("delete_current_group_goes_to_parent", async ({ page }) => {
  await createVault(page);
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    const homelab = await api.upsertEntity("group", { name: "Homelab" });
    await api.upsertEntity("group", { name: "Proxmox", parentId: homelab.id });
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  await openGroupPath(page, "Homelab / Proxmox");
  await expect(here(page)).toHaveText("Proxmox");

  await currentGroupAction(page, "Proxmox", "Delete group");
  await page.getByRole("dialog", { name: "Delete group Proxmox" }).getByRole("button", { name: "Delete", exact: true }).click();
  await expect(here(page)).toHaveText("Homelab");
  await expect(page.getByText("This group is empty.")).toBeVisible();
});

test("current_group_deleted_by_sync", async ({ page }) => {
  await createVault(page);
  const proxmoxId = await page.evaluate(async () => {
    const api = window.__tetherMock;
    const homelab = await api.upsertEntity("group", { name: "Homelab" });
    return (await api.upsertEntity("group", { name: "Proxmox", parentId: homelab.id })).id;
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  await openGroupPath(page, "Homelab / Proxmox");
  await expect(here(page)).toHaveText("Proxmox");

  await page.evaluate((id) => window.__tetherMock.control.remoteDelete("group", id), proxmoxId);
  await expect(here(page)).toHaveText("Homelab");
  await expect(breadcrumb(page)).not.toContainText("Proxmox");
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("host_with_missing_group_shows_at_top_level", async ({ page }) => {
  await createVault(page);
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    const lost = await api.upsertEntity("group", { name: "Lost" });
    await api.upsertEntity("host", { label: "stray", address: "10.0.0.9", groupId: lost.id });
    // Sync can leave a host pointing at a group that is gone; the mock refuses that, so hide the group instead.
    const list = api.listEntities.bind(api);
    api.listEntities = (async (kind: Parameters<typeof list>[0]) => {
      const records = await list(kind);
      return kind === "group" ? records.filter((g) => g.id !== lost.id) : records;
    }) as typeof list;
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  await expect(folders(page)).toHaveCount(0);
  await expect(hostRow(page, "stray")).toBeVisible();
  await expect(page.getByText("No hosts yet. Use New host to add one.")).toHaveCount(0);
});

test("current_group_reparented_then_deleted_by_sync_goes_to_new_parent", async ({ page }) => {
  await createVault(page);
  const ids = await page.evaluate(async () => {
    const api = window.__tetherMock;
    const homelab = await api.upsertEntity("group", { name: "Homelab" });
    const office = await api.upsertEntity("group", { name: "Office" });
    const rack = await api.upsertEntity("group", { name: "Rack", parentId: homelab.id });
    return { office: office.id, rack: rack.id };
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  await openGroupPath(page, "Homelab / Rack");

  // Moved under Office elsewhere, then deleted elsewhere: the page falls back to Office.
  await page.evaluate(
    ({ office, rack }) => window.__tetherMock.control.remoteUpsert("group", { id: rack, name: "Rack", parentId: office }),
    ids,
  );
  await expect(breadcrumb(page)).toContainText("Office");
  await page.evaluate((id) => window.__tetherMock.control.remoteDelete("group", id), ids.rack);
  await expect(here(page)).toHaveText("Office");
});

test("double_click_card_connects_once", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web", "10.0.0.5");
  await hostRow(page, "web").dblclick();
  await expect(terminalTabs(page)).toHaveCount(1);
  await expect.poll(() => sessionIds(page)).toHaveLength(1);
  // A second session would have opened by now.
  await page.waitForTimeout(300);
  expect(await sessionIds(page)).toHaveLength(1);
  await expect(terminalTabs(page)).toHaveCount(1);
});

test("empty_group_state", async ({ page }) => {
  await createVault(page);
  await expect(page.getByText("No hosts yet. Use New host to add one.")).toBeVisible();
  await createGroup(page, "Empty");
  await openGroupPath(page, "Empty");
  await expect(page.getByText("This group is empty.")).toBeVisible();
  await expect(hostList(page)).toHaveCount(0);

  // Its own New host starts in the group.
  const empty = page.locator(".hosts-empty");
  await empty.getByRole("button", { name: "New host" }).click();
  await expect(
    hostForm(page).getByLabel("Group", { exact: true }).locator("option:checked"),
  ).toHaveText("Empty");
});

test("host_editor_drops_deleted_group", async ({ page }) => {
  await createVault(page);
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    const g = await api.upsertEntity("group", { name: "Staging" });
    await api.upsertEntity("host", { label: "stage1", address: "10.0.0.31", groupId: g.id });
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  await openGroupPath(page, "Staging");
  await editHost(page, "stage1");
  const form = hostForm(page);
  await expect(form.getByLabel("Group", { exact: true })).not.toHaveValue("");

  // The group is deleted while the host is open (its hosts move up).
  await currentGroupAction(page, "Staging", "Delete group");
  await page.getByRole("dialog", { name: "Delete group Staging" }).getByRole("button", { name: "Delete", exact: true }).click();
  await expect(here(page)).toHaveText("All hosts");
  await expect(folders(page)).toHaveCount(0);

  // The editor follows, and saving works.
  await expect(form.getByLabel("Group", { exact: true })).toHaveValue("");
  await form.getByLabel("Label").fill("stage-1");
  await form.getByRole("button", { name: "Save host" }).click();
  await expect(hostRow(page, "stage-1")).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
});

// -- the Hosts page keeps its place while another view shows ----------------------

/** A host `web` in the group Homelab, able to connect; the page is left at the top level. */
async function seedHomelabWeb(page: Page) {
  await page.evaluate(async () => {
    const api = window.__tetherMock;
    const identity = await api.upsertEntity("identity", { label: "deploy", username: "deploy", password: "pw" });
    const group = await api.upsertEntity("group", { name: "Homelab" });
    await api.upsertEntity("host", {
      label: "web",
      address: "10.0.0.5",
      groupId: group.id,
      settings: { identityId: identity.id },
    });
  });
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
}

/** Homelab open, filter `we`, and the editor on `web` with an unsaved Label `web-2`. */
async function leaveHostsMidEdit(page: Page) {
  await seedHomelabWeb(page);
  await openGroupPath(page, "Homelab");
  await filterBox(page).fill("we");
  await editHost(page, "web");
  await hostForm(page).getByLabel("Label").fill("web-2");
}

async function expectHostsRemembered(page: Page) {
  await expect(here(page)).toHaveText("Homelab");
  await expect(filterBox(page)).toHaveValue("we");
  await expect(hostForm(page).getByLabel("Label")).toHaveValue("web-2");
}

test("hosts_state_survives_connect", async ({ page }) => {
  await createVault(page);
  await leaveHostsMidEdit(page);
  // Hover again: opening the editor can reflow the card out from under the mouse.
  await hostCard(page, "web").hover();
  await page.getByRole("button", { name: "Connect to web", exact: true }).click();
  await expect(page.getByRole("region", { name: "Terminals" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Hosts", level: 1 })).toBeHidden();

  await goTo(page, "Hosts");
  await expectHostsRemembered(page);
});

test("hosts_state_survives_other_pages", async ({ page }) => {
  await createVault(page);
  await leaveHostsMidEdit(page);
  await goTo(page, "Settings");
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeVisible();

  await goTo(page, "Hosts");
  await expectHostsRemembered(page);
});

test("lock_drops_hosts_state", async ({ page }) => {
  await createVault(page);
  await leaveHostsMidEdit(page);
  await goTo(page, "Settings");
  await goTo(page, "Hosts");
  await expectHostsRemembered(page);

  await page.getByRole("button", { name: "Lock vault" }).click();
  await expect(page.getByRole("heading", { name: "Unlock Termlyx" })).toBeVisible();
  await page.getByLabel("Master password").fill(PASSWORD);
  await page.getByRole("button", { name: "Unlock", exact: true }).click();

  await expect(here(page)).toHaveText("All hosts");
  await expect(breadcrumb(page)).toHaveText("All hosts");
  await expect(hostForm(page)).toHaveCount(0);
  await expect(filterBox(page)).toHaveValue("");
});

test("hosts_picked_in_search_opens_from_another_page", async ({ page }) => {
  await createVault(page);
  await seedHomelabWeb(page);
  await goTo(page, "Settings");
  await page.keyboard.press("Meta+k");
  await page.getByRole("dialog").getByRole("combobox").fill("web");
  await page.keyboard.press("Alt+Enter");

  await expect(here(page)).toHaveText("Homelab");
  await expect(hostForm(page).getByLabel("Label")).toHaveValue("web");
});

test("password_field_survives_without_remount", async ({ page }) => {
  await createVault(page);
  await seedHomelabWeb(page);
  await openGroupPath(page, "Homelab");
  await editHost(page, "web");
  const form = hostForm(page);
  await form.getByLabel("Identity", { exact: true }).selectOption({ label: "Own username & password" });
  const password = form.getByLabel("Password", { exact: true });
  await password.fill("typed-secret");
  await holdFields(password);

  // Hover again: opening the editor can reflow the card out from under the mouse.
  await hostCard(page, "web").hover();
  await page.getByRole("button", { name: "Connect to web", exact: true }).click();
  await expect(page.getByRole("region", { name: "Terminals" })).toBeVisible();
  await goTo(page, "Hosts");

  // Not copied out and back: the very same input, still holding what was typed.
  expect(await heldValues(page)).toEqual(["typed-secret"]);
  expect(
    await form.getByLabel("Password", { exact: true }).evaluate((el) => el === window.__fields![0]),
  ).toBe(true);
});

test("saving_a_host_closes_the_editor_and_shows_the_change", async ({ page }) => {
  await createVault(page);
  await createHost(page, { label: "web-1", address: "web1.example" });
  await editHost(page, "web-1");
  const form = hostForm(page);
  await form.getByLabel("Label").fill("web-renamed");
  await form.getByLabel("Address").fill("web9.example");
  await form.getByRole("button", { name: "Save host" }).click();

  // The editor goes, and the card behind it has the new values.
  await expect(form).toBeHidden();
  await expect(hostRow(page, "web-renamed")).toContainText("web9.example");
  await expect(hostRow(page, "web-1")).toHaveCount(0);

  // A new host closes it too.
  await hostsBar(page).getByRole("button", { name: "New host" }).click();
  await form.getByLabel("Label").fill("db-1");
  await form.getByLabel("Address").fill("db1.example");
  await form.getByRole("button", { name: "Save host" }).click();
  await expect(form).toBeHidden();
  await expect(hostRow(page, "db-1")).toBeVisible();
});
