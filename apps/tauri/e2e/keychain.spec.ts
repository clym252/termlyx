import { expect, test } from "./fixtures";
import {
  PASSWORD,
  createVault,
  generateKey,
  goTo,
  heldValues,
  holdFields,
  identityList,
  keyList,
  rowOf,
  ruleList,
  seedHost,
} from "./helpers";

test("generate_ed25519_shows_fingerprint_and_copy_public", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await createVault(page);
  await goTo(page, "Keychain");
  await expect(page.getByRole("heading", { name: "Keychain", level: 1 })).toBeVisible();
  await expect(page.getByText("No keys yet")).toBeVisible();

  await page.getByRole("button", { name: "Generate key" }).click();
  const dialog = page.getByRole("dialog", { name: "Generate key" });
  // ed25519 is the default.
  await expect(dialog.getByLabel("Algorithm")).toHaveValue("ed25519");
  await dialog.getByLabel("Label").fill("laptop");
  const passphrase = dialog.getByLabel("Passphrase");
  await passphrase.fill("hunter2 hunter2");
  await holdFields(passphrase);
  await dialog.getByRole("button", { name: "Generate", exact: true }).click();

  // The result: the fingerprint, and the public key to copy.
  await expect(dialog.getByRole("status")).toHaveText("Key generated");
  const fingerprint = await dialog.getByTestId("fingerprint").innerText();
  expect(fingerprint).toMatch(/^SHA256:/);
  const stored = await page.evaluate(async () => (await window.__tetherMock.listEntities("ssh_key"))[0]);
  expect(fingerprint).toBe(stored.fingerprint);
  expect(stored.hasPassphrase).toBe(true);
  // The passphrase was read at submit and is gone from the form.
  expect(await heldValues(page)).toEqual([""]);

  await dialog.getByRole("button", { name: "Copy public key" }).click();
  await expect(dialog.getByRole("status")).toHaveText("Public key copied");
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toBe(stored.publicOpenssh);
  expect(copied).toMatch(/^ssh-ed25519 /);
  expect(copied).not.toContain("PRIVATE");

  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(dialog).toBeHidden();
  const row = rowOf(keyList(page), "laptop");
  await expect(row).toContainText("ssh-ed25519");
  await expect(row).toContainText(stored.fingerprint);
  await expect(row).toContainText("Passphrase");
});

test("import_key_needs_passphrase", async ({ page }) => {
  await createVault(page);
  // An encrypted key, as another machine would have it.
  const pem = await page.evaluate(async (pw) => {
    const api = window.__tetherMock;
    const k = await api.keyGenerate("ed25519", "source", "s3cret phrase");
    const { privateOpenssh } = await api.keyExportPrivate(k.id, pw);
    await api.deleteEntity("ssh_key", k.id);
    return privateOpenssh;
  }, PASSWORD);

  await goTo(page, "Keychain");
  await expect(page.getByText("No keys yet")).toBeVisible();
  await page.getByRole("button", { name: "Import key" }).click();
  const dialog = page.getByRole("dialog", { name: "Import key" });
  const text = dialog.getByLabel("Private key");
  await text.fill(pem);
  await dialog.getByLabel("Label").fill("from backup");
  await dialog.getByRole("button", { name: "Import", exact: true }).click();

  // It is encrypted: the dialog asks for the passphrase and keeps the key text for the retry.
  await expect(dialog.getByRole("status")).toContainText("encrypted");
  const passphrase = dialog.getByLabel("Key passphrase");
  await expect(passphrase).toBeVisible();
  await expect(text).toHaveValue(pem);
  await holdFields(passphrase, text);

  // A wrong passphrase is refused, cleared, and asked for again.
  await passphrase.fill("not it");
  await dialog.getByRole("button", { name: "Import", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText("Wrong passphrase");
  await expect(passphrase).toHaveValue("");
  await expect(keyList(page).getByRole("listitem")).toHaveCount(0);

  await passphrase.fill("s3cret phrase");
  await dialog.getByRole("button", { name: "Import", exact: true }).click();
  await expect(dialog).toBeHidden();
  const row = rowOf(keyList(page), "from backup");
  await expect(row).toBeVisible();
  await expect(row).toContainText("Passphrase");
  // Neither the passphrase nor the key text outlives the dialog.
  expect(await heldValues(page)).toEqual(["", ""]);

  // Importing a key file works too (the file's text fills the same field).
  await page.getByRole("button", { name: "Import key" }).click();
  const second = page.getByRole("dialog", { name: "Import key" });
  await second.getByLabel("Key file").setInputFiles({
    name: "id_plain",
    mimeType: "text/plain",
    buffer: Buffer.from("-----BEGIN OPENSSH PRIVATE KEY-----\nplainbody\n-----END OPENSSH PRIVATE KEY-----\n"),
  });
  await expect(second.getByLabel("Private key")).toHaveValue(/plainbody/);
  await second.getByLabel("Label").fill("plain one");
  await second.getByRole("button", { name: "Import", exact: true }).click();
  await expect(second).toBeHidden();
  await expect(rowOf(keyList(page), "plain one")).toBeVisible();

  // Not a key at all: an error, and the dialog stays.
  await page.getByRole("button", { name: "Import key" }).click();
  const bad = page.getByRole("dialog", { name: "Import key" });
  await bad.getByLabel("Private key").fill("hello");
  await bad.getByRole("button", { name: "Import", exact: true }).click();
  await expect(bad.getByRole("alert")).toContainText("not a recognised private key");
});

test("identity_links_key", async ({ page }) => {
  await createVault(page);
  await generateKey(page, "deploy key");

  await page.getByRole("button", { name: "New identity" }).click();
  const dialog = page.getByRole("dialog", { name: "New identity" });
  await dialog.getByLabel("Label").fill("ops");
  await dialog.getByLabel("Username").fill("root");
  await dialog.getByLabel("Key", { exact: true }).selectOption({ label: "deploy key" });
  await dialog.getByRole("button", { name: "Save identity" }).click();
  await expect(dialog).toBeHidden();
  const row = rowOf(identityList(page), "ops");
  await expect(row).toContainText("root");
  await expect(row).toContainText("deploy key");
  const ids = await page.evaluate(async () => ({
    identity: (await window.__tetherMock.listEntities("identity"))[0],
    key: (await window.__tetherMock.listEntities("ssh_key"))[0],
  }));
  expect(ids.identity.keyId).toBe(ids.key.id);

  // A host uses the identity: both it and the key it names are protected.
  await page.evaluate(async (identityId) => {
    await window.__tetherMock.upsertEntity("host", {
      label: "web-1",
      address: "10.0.0.5",
      settings: { identityId },
    });
  }, ids.identity.id);

  await page.getByRole("button", { name: "Delete key deploy key" }).click();
  let confirm = page.getByRole("dialog", { name: "Delete key deploy key" });
  await confirm.getByRole("button", { name: "Delete", exact: true }).click();
  let blocked = page.getByRole("dialog", { name: "Cannot delete deploy key" });
  await expect(blocked.getByRole("list", { name: "Still used by" }).getByRole("listitem")).toHaveText([
    "Identity ops",
  ]);
  await blocked.getByRole("button", { name: "Close" }).click();
  await expect(rowOf(keyList(page), "deploy key")).toBeVisible();

  await page.getByRole("button", { name: "Delete identity ops" }).click();
  confirm = page.getByRole("dialog", { name: "Delete identity ops" });
  await confirm.getByRole("button", { name: "Delete", exact: true }).click();
  blocked = page.getByRole("dialog", { name: "Cannot delete ops" });
  await expect(blocked.getByRole("list", { name: "Still used by" }).getByRole("listitem")).toHaveText([
    "Host web-1",
  ]);
  await blocked.getByRole("button", { name: "Close" }).click();
  await expect(rowOf(identityList(page), "ops")).toBeVisible();

  // Once the host lets go, both can be deleted (identity first).
  await page.evaluate(async () => {
    const [host] = await window.__tetherMock.listEntities("host");
    await window.__tetherMock.upsertEntity("host", {
      id: host.id,
      label: host.label,
      address: host.address,
    });
  });
  await page.getByRole("button", { name: "Delete identity ops" }).click();
  await page
    .getByRole("dialog", { name: "Delete identity ops" })
    .getByRole("button", { name: "Delete", exact: true })
    .click();
  await expect(page.getByText("No identities yet")).toBeVisible();
  await page.getByRole("button", { name: "Delete key deploy key" }).click();
  await page
    .getByRole("dialog", { name: "Delete key deploy key" })
    .getByRole("button", { name: "Delete", exact: true })
    .click();
  await expect(page.getByText("No keys yet")).toBeVisible();
});

test("import_known_hosts_file", async ({ page }) => {
  await createVault(page);
  const file = "/home/me/imported_known_hosts";
  await page.evaluate((file) => {
    window.__tetherMock.control.setFile(
      file,
      [
        "# a comment",
        "example.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIexample",
        "[git.example.com]:2222 ecdsa-sha2-nistp256 AAAAE2VjZHNhgit",
        "",
      ].join("\n"),
    );
    window.__tetherMock.control.setPick(null);
  }, file);

  await goTo(page, "Known Hosts");
  await expect(page.getByRole("heading", { name: "Known Hosts", level: 1 })).toBeVisible();
  await expect(page.getByText("No known hosts yet")).toBeVisible();

  // A cancelled picker changes nothing.
  await page.getByRole("button", { name: "Import known_hosts…" }).click();
  await expect(page.getByRole("status")).toHaveCount(0);
  await expect(page.getByRole("alert")).toHaveCount(0);

  await page.evaluate((file) => window.__tetherMock.control.setPick(file), file);
  await page.getByRole("button", { name: "Import known_hosts…" }).click();
  await expect(page.getByRole("status")).toHaveText("2 added, 0 already known");
  const table = page.getByRole("table", { name: "Known hosts" });
  const rows = table.getByRole("row");
  await expect(rows).toHaveCount(3); // header + 2
  const example = rows.filter({ hasText: "example.com" }).filter({ hasNotText: "git." });
  await expect(example).toContainText("22");
  await expect(example).toContainText("ssh-ed25519");
  await expect(example).toContainText(/SHA256:/);
  await expect(example).toContainText("import");
  const git = rows.filter({ hasText: "git.example.com" });
  await expect(git).toContainText("2222");
  await expect(git).toContainText("ecdsa-sha2-nistp256");

  // Again: nothing new.
  await page.getByRole("button", { name: "Import known_hosts…" }).click();
  await expect(page.getByRole("status")).toHaveText("0 added, 2 already known");
  await expect(rows).toHaveCount(3);

  // Removing an entry asks first; the next connection to that host will ask again.
  await page.getByRole("button", { name: "Delete known host example.com" }).click();
  const confirm = page.getByRole("dialog", { name: "Remove known host example.com" });
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(rows).toHaveCount(3);
  await page.getByRole("button", { name: "Delete known host example.com" }).click();
  await page
    .getByRole("dialog", { name: "Remove known host example.com" })
    .getByRole("button", { name: "Remove" })
    .click();
  await expect(rows).toHaveCount(2);
  await expect(example).toHaveCount(0);
  await expect(git).toHaveCount(1);
});

test("forward_rule_create_start_shows_active_then_error_on_conflict", async ({ page }) => {
  await createVault(page);
  await seedHost(page, "web-1", "10.0.0.5");
  await goTo(page, "Port Forwarding");
  await expect(page.getByRole("heading", { name: "Port Forwarding", level: 1 })).toBeVisible();
  await expect(page.getByText("No forwarding rules yet")).toBeVisible();

  async function addRule(label: string, bindPort: string) {
    await page.getByRole("button", { name: "New rule" }).click();
    const dialog = page.getByRole("dialog", { name: "New rule" });
    await dialog.getByLabel("Host", { exact: true }).selectOption({ label: "web-1" });
    await dialog.getByLabel("Label").fill(label);
    await expect(dialog.getByLabel("Type")).toHaveValue("local");
    await expect(dialog.getByLabel("Bind address")).toHaveValue("127.0.0.1");
    await dialog.getByLabel("Bind port").fill(bindPort);
    await dialog.getByLabel("Destination host").fill("db.internal");
    await dialog.getByLabel("Destination port").fill("5432");
    await dialog.getByRole("button", { name: "Save rule" }).click();
    await expect(dialog).toBeHidden();
    await expect(rowOf(ruleList(page), label)).toBeVisible();
  }

  await addRule("db", "15432");
  const db = rowOf(ruleList(page), "db");
  await expect(db).toContainText("Local");
  await expect(db).toContainText("127.0.0.1:15432 → db.internal:5432");
  await expect(db).toContainText("web-1");
  await expect(db.getByRole("status")).toHaveText("Stopped");

  await db.getByRole("button", { name: "Start" }).click();
  await expect(db.getByRole("status")).toHaveText("Active · 127.0.0.1:15432");
  await expect(db.getByRole("button", { name: "Stop" })).toBeVisible();

  // A second rule on the same address: Error, the first stays up.
  await addRule("db copy", "15432");
  const copy = rowOf(ruleList(page), "db copy");
  await copy.getByRole("button", { name: "Start" }).click();
  await expect(copy.getByRole("status")).toContainText("address in use");
  await expect(copy.getByRole("status")).toContainText("Error");
  await expect(db.getByRole("status")).toHaveText("Active · 127.0.0.1:15432");

  // Statuses are per rule: an event for one never changes the other.
  const ids = await page.evaluate(async () => {
    const rules = await window.__tetherMock.listEntities("forward_rule");
    return Object.fromEntries(rules.map((r) => [r.label, r.id]));
  });
  await page.evaluate(
    (id) => window.__tetherMock.control.emitForward({ ruleId: id, status: { state: "starting" } }),
    ids["db copy"],
  );
  await expect(copy.getByRole("status")).toHaveText("Starting…");
  await expect(db.getByRole("status")).toHaveText("Active · 127.0.0.1:15432");
  await page.evaluate(
    (id) => window.__tetherMock.control.emitForward({ ruleId: id, status: { state: "stopped" } }),
    ids.db,
  );
  await expect(db.getByRole("status")).toHaveText("Stopped");
  await expect(copy.getByRole("status")).toHaveText("Starting…");

  // With the address free, the second rule starts.
  await page.evaluate(
    (id) => window.__tetherMock.control.emitForward({ ruleId: id, status: { state: "stopped" } }),
    ids["db copy"],
  );
  await copy.getByRole("button", { name: "Start" }).click();
  await expect(copy.getByRole("status")).toHaveText("Active · 127.0.0.1:15432");
  await copy.getByRole("button", { name: "Stop" }).click();
  await expect(copy.getByRole("status")).toHaveText("Stopped");
});

test("forward_start_error_survives_other_rules_events", async ({ page }) => {
  await createVault(page);
  const hostId = await seedHost(page, "web-1", "10.0.0.5");
  const [a] = await page.evaluate(async (hostId) => {
    const api = window.__tetherMock;
    const rule = (label: string, bindPort: number) =>
      api.upsertEntity("forward_rule", {
        hostId,
        label,
        kind: "local",
        bindAddr: "127.0.0.1",
        bindPort,
        destHost: "db.internal",
        destPort: 5432,
        autostart: false,
      });
    return [(await rule("alpha", 15001)).id, (await rule("beta", 15002)).id];
  }, hostId);
  await goTo(page, "Port Forwarding");
  const alpha = rowOf(ruleList(page), "alpha");
  const beta = rowOf(ruleList(page), "beta");

  // Alpha has a status from earlier in this run.
  await alpha.getByRole("button", { name: "Start" }).click();
  await expect(alpha.getByRole("status")).toContainText("Active");
  await alpha.getByRole("button", { name: "Stop" }).click();
  await expect(alpha.getByRole("status")).toHaveText("Stopped");

  // Its next start fails before any status: the error shows...
  await page.evaluate((id) => window.__tetherMock.control.rejectNextForwardStart(id, "login refused"), a);
  await alpha.getByRole("button", { name: "Start" }).click();
  await expect(alpha.getByRole("status")).toContainText("login refused");
  // ...and stays while another rule changes.
  await beta.getByRole("button", { name: "Start" }).click();
  await expect(beta.getByRole("status")).toContainText("Active");
  await expect(alpha.getByRole("status")).toContainText("login refused");

  // Alpha's own next event replaces it.
  await alpha.getByRole("button", { name: "Start" }).click();
  await expect(alpha.getByRole("status")).toContainText("Active");
});
