import { describe, expect, it } from "vitest";
import bindings from "./bindings.json";
import syncErrorCodes from "./sync-error-codes.json";
import { createMockApi, MOCK_DEVICE_ID, MOCK_FINGERPRINT, MOCK_SERVER } from "./mock";
import {
  DTO_FIELDS,
  isApiError,
  recoveryKeyOf,
  type ApiError,
  type SyncStatus,
  type TetherApi,
  type VaultLocked,
} from "./types";

/** The rejection of `p`, which must be an ApiError. */
async function apiError(p: Promise<unknown>): Promise<ApiError> {
  try {
    await p;
  } catch (e) {
    if (isApiError(e)) return e;
    throw new Error(`not an ApiError: ${String(e)}`);
  }
  throw new Error("expected a rejection");
}

describe("bindings", () => {
  // Rust's cmd_dto_roundtrip checks the same file against the serde DTOs, so
  // a renamed field on either side fails a test.
  it("types_match_rust_bindings", () => {
    const rust = bindings as Record<string, string[]>;
    expect(Object.keys(DTO_FIELDS).sort()).toEqual(Object.keys(rust).sort());
    for (const [name, fields] of Object.entries(DTO_FIELDS)) {
      expect([...fields].sort(), name).toEqual([...rust[name]].sort());
    }
  });
});

/** The CRUD scenario every TetherApi implementation must pass. */
async function crudScenario(api: TetherApi) {
  expect((await api.vaultStatus()).state).toBe("uninitialized");
  const { recoveryKey } = await api.vaultCreate("master-pw");
  expect(recoveryKey.length).toBeGreaterThan(0);
  expect((await api.vaultStatus()).state).toBe("unlocked");

  let locked = 0;
  const unlisten = await api.onVaultLocked(() => locked++);
  await api.vaultLock();
  expect(locked).toBe(1);
  unlisten();
  expect((await api.vaultStatus()).state).toBe("locked");
  expect((await apiError(api.vaultUnlock("wrong"))).code).toBe("wrongPassword");
  expect((await apiError(api.listEntities("host"))).code).toBe("locked");
  await api.vaultUnlock("master-pw");

  const group = await api.upsertEntity("group", {
    name: "prod",
    parentId: null,
    settings: { port: 2200 },
  });
  const identity = await api.upsertEntity("identity", {
    label: "ops",
    username: "root",
    password: "canary-pw",
  });
  expect(identity.hasPassword).toBe(true);
  expect(JSON.stringify(identity)).not.toContain("canary-pw");
  const tag = await api.upsertEntity("tag", { name: "db", color: "#f00" });
  const host = await api.upsertEntity("host", {
    label: "db 🐘 数据库",
    address: "10.0.0.5",
    groupId: group.id,
    tagIds: [tag.id],
    settings: { identityId: identity.id },
  });
  expect(host.label).toBe("db 🐘 数据库");
  expect(host.forwardRuleIds).toEqual([]);

  const hosts = await api.listEntities("host");
  expect(hosts.map((h) => h.id)).toEqual([host.id]);
  expect(JSON.stringify(await api.listEntities("identity"))).not.toContain("canary-pw");

  // Update keeps the password unless it is set or cleared.
  const renamed = await api.upsertEntity("identity", {
    id: identity.id,
    label: "ops2",
    username: "root",
  });
  expect(renamed.hasPassword).toBe(true);
  const cleared = await api.upsertEntity("identity", {
    id: identity.id,
    label: "ops2",
    username: "root",
    password: null,
  });
  expect(cleared.hasPassword).toBe(false);

  // Effective settings name the level each value came from.
  const eff = await api.effectiveSettings(host.id);
  expect(eff.port).toEqual({ value: 2200, source: { kind: "group", groupId: group.id } });
  expect(eff.identityId).toEqual({ value: identity.id, source: { kind: "host" } });
  expect(eff.termType).toEqual({ value: "xterm-256color", source: { kind: "default" } });

  // Referenced records are not deleted; the error lists who uses them.
  const refErr = await apiError(api.deleteEntity("identity", identity.id));
  expect(refErr.code).toBe("referenced");
  expect(refErr.refs).toEqual([host.id]);
  const groupErr = await apiError(api.deleteEntity("group", group.id));
  expect(groupErr.code).toBe("referenced");
  expect(groupErr.refs).toEqual([host.id]);
  await api.deleteGroup(group.id, "moveToParent");
  expect((await api.listEntities("host"))[0].groupId).toBeNull();

  // Forward rules follow their host; deleting the host deletes them.
  const rule = await api.upsertEntity("forward_rule", {
    hostId: host.id,
    label: "web",
    kind: "local",
    bindAddr: "127.0.0.1",
    bindPort: 8080,
    destHost: "localhost",
    destPort: 80,
    autostart: false,
  });
  expect((await api.listEntities("host"))[0].forwardRuleIds).toEqual([rule.id]);

  // Deleting a tag strips it from hosts.
  await api.deleteEntity("tag", tag.id);
  expect((await api.listEntities("host"))[0].tagIds).toEqual([]);

  await api.deleteEntity("host", host.id);
  expect(await api.listEntities("forward_rule")).toEqual([]);
  await api.deleteEntity("identity", identity.id);
  expect(await api.listEntities("identity")).toEqual([]);

  // Missing records and bad references are errors, not silent no-ops.
  expect((await apiError(api.deleteEntity("host", host.id))).code).toBe("notFound");
  expect(
    (await apiError(api.upsertEntity("host", { label: "x", address: "y", groupId: group.id })))
      .code,
  ).toBe("missingRef");

  // Recovery resets the password.
  await api.vaultLock();
  expect((await apiError(api.vaultUnlockRecovery("bogus", "new-pw"))).code).toBe(
    "wrongRecoveryKey",
  );
  await api.vaultUnlockRecovery(recoveryKey, "new-pw");
  await api.vaultLock();
  await api.vaultUnlock("new-pw");
}

describe.each([["mock", () => createMockApi()]])("TetherApi contract (%s)", (_name, make) => {
  it("crud_scenario", async () => {
    await crudScenario(make());
  });

  it("own_logins_are_linked_inherited_and_pruned", async () => {
    const api = createMockApi();
    await api.vaultCreate("pw");
    const group = await api.upsertEntity("group", { name: "Production" });
    const host = await api.upsertEntity("host", { label: "web", address: "10.0.0.5", groupId: group.id });
    const own = await api.setOwnLogin({ ownerId: group.id, username: "alice", password: "lab" });
    expect(own).toMatchObject({ label: "Production", username: "alice", hasPassword: true, owner: group.id });
    expect((await api.effectiveSettings(host.id)).identityId).toEqual({ value: own.id, source: { kind: "group", groupId: group.id } });
    // Omitting the password keeps it; the same identity is updated.
    const again = await api.setOwnLogin({ ownerId: group.id, username: "root" });
    expect(again).toMatchObject({ id: own.id, username: "root", hasPassword: true });
    // The group stops using it: it goes.
    await api.upsertEntity("group", { id: group.id, name: "Production", settings: { identityId: null } });
    expect(await api.listEntities("identity")).toEqual([]);
    expect((await apiError(api.setOwnLogin({ ownerId: "nope", username: "x" }))).code).toBe("missingRef");
  });

  it("keys_never_return_private_material_except_export", async () => {
    const api = make();
    await api.vaultCreate("pw");
    const key = await api.keyGenerate("ed25519", "laptop");
    expect(key.fingerprint).toMatch(/^SHA256:/);
    expect(JSON.stringify(await api.listEntities("ssh_key"))).not.toContain("PRIVATE KEY");
    const exported = await api.keyExportPrivate(key.id);
    expect(exported.privateOpenssh).toContain("PRIVATE KEY");
    const identity = await api.upsertEntity("identity", {
      label: "k",
      username: "u",
      keyId: key.id,
    });
    const err = await apiError(api.deleteEntity("ssh_key", key.id));
    expect(err.refs).toEqual([identity.id]);
  });

  it("key_passphrase_kept_only_for_encrypted_keys", async () => {
    const api = make();
    await api.vaultCreate("pw");
    const plain = await api.keyGenerate("ed25519", "plain");
    const plainText = (await api.keyExportPrivate(plain.id)).privateOpenssh;
    // A passphrase for an unencrypted key is ignored.
    const reimported = await api.keyImport(plainText, "ignored");
    expect(reimported.hasPassphrase).toBe(false);
    expect(reimported.fingerprint).toBe(plain.fingerprint);

    const locked = await api.keyGenerate("ed25519", "locked", "kp");
    expect(locked.hasPassphrase).toBe(true);
    const lockedText = (await api.keyExportPrivate(locked.id)).privateOpenssh;
    expect((await apiError(api.keyImport(lockedText))).code).toBe("needsPassphrase");
    expect((await apiError(api.keyImport(lockedText, "nope"))).code).toBe("badPassphrase");
    const again = await api.keyImport(lockedText, "kp");
    expect(again.hasPassphrase).toBe(true);
    expect(again.fingerprint).toBe(locked.fingerprint);
  });

  it("snippets_crud", async () => {
    const api = make();
    await api.vaultCreate("pw");
    const made = await api.upsertEntity("snippet", { label: "Disk usage", text: "df -h" });
    expect(made.run).toBe(false);
    expect(await api.listEntities("snippet")).toEqual([made]);
    const edited = await api.upsertEntity("snippet", { ...made, text: "df -h /", run: true });
    expect(edited).toEqual({ ...made, text: "df -h /", run: true });
    await api.deleteEntity("snippet", made.id);
    expect(await api.listEntities("snippet")).toEqual([]);
  });

  it("upsert_refuses_unknown_fields", async () => {
    const api = make();
    await api.vaultCreate("pw");
    const bad = (input: unknown) =>
      apiError(api.upsertEntity("host", input as Parameters<typeof api.upsertEntity<"host">>[1]));
    expect((await bad({ label: "h", address: "a", nickname: "x" })).code).toBe("invalid");
    expect((await bad({ label: "h", address: "a", settings: { colour: "red" } })).code).toBe(
      "invalid",
    );
    expect(await api.listEntities("host")).toEqual([]);
  });

  it("locking_an_uncreated_vault_succeeds", async () => {
    const api = make();
    let locked = 0;
    await api.onVaultLocked(() => locked++);
    await api.vaultLock();
    expect(locked).toBe(1);
    expect((await api.vaultStatus()).state).toBe("uninitialized");
  });
});

describe("mock auto-lock (like autolock.rs)", () => {
  it("locks_on_return_after_the_limit_only", async () => {
    const api = createMockApi();
    await api.vaultCreate("master-pw");
    const reasons: string[] = [];
    await api.onVaultLocked((e) => reasons.push(e.reason));

    // Never: no lock however long away.
    expect(await api.appFront(false)).toBe(false);
    api.control.advanceClock(24 * 3600_000);
    expect(await api.appFront(true)).toBe(false);

    await api.setAutoLock(5);
    expect(api.control.autoLockMinutes()).toBe(5);
    await api.appFront(false);
    api.control.advanceClock(4 * 60_000);
    expect(await api.appFront(true)).toBe(false);
    await api.appFront(false);
    api.control.advanceClock(2 * 60_000);
    // A second report while away keeps the first time.
    await api.appFront(false);
    api.control.advanceClock(3 * 60_000);
    expect(await api.appFront(true)).toBe(true);
    expect((await api.vaultStatus()).state).toBe("locked");
    expect(reasons).toEqual(["idle"]);

    // Already locked: no second event.
    await api.appFront(false);
    api.control.advanceClock(10 * 60_000);
    expect(await api.appFront(true)).toBe(true);
    expect(reasons).toEqual(["idle"]);
    expect((await apiError(api.setAutoLock(0))).code).toBe("invalid");
  });
});

describe("mock event hooks", () => {
  it("prompts_round_trip_and_withdraw", async () => {
    const api = createMockApi();
    const seen: string[] = [];
    const withdrawn: string[] = [];
    await api.onPrompt((e) => seen.push(e.id));
    await api.onPromptWithdrawn((e) => withdrawn.push(e.id));
    const answer = api.control.ask({ kind: "password", user: "u", host: "h", saveTo: null });
    expect(seen).toHaveLength(1);
    await api.promptRespond(seen[0], { secret: "pw" });
    expect(await answer).toEqual({ secret: "pw" });

    const unanswered = api.control.ask({ kind: "passphrase", keyLabel: "k" });
    api.control.withdraw(seen[1]);
    expect(withdrawn).toEqual([seen[1]]);
    expect(await unanswered).toBe("reject");
  });

  it("sessions_are_owned_and_closed_together", async () => {
    const api = createMockApi();
    const out: Uint8Array[] = [];
    const closed: (string | null)[] = [];
    const handlers = { onOutput: (b: Uint8Array) => out.push(b), onClosed: (r: string | null) => closed.push(r) };
    const a = await api.openSession({ kind: "local" }, { cols: 80, rows: 24 }, handlers);
    await api.openSession({ kind: "local", cwd: "/tmp" }, { cols: 80, rows: 24 }, handlers);
    api.control.output(a.sessionId, new Uint8Array([104, 105]));
    expect(out).toEqual([new Uint8Array([104, 105])]);
    await api.write(a.sessionId, new Uint8Array([108, 115]));
    expect(api.control.written(a.sessionId)).toEqual([new Uint8Array([108, 115])]);
    expect((await api.activeWork()).sessions).toHaveLength(2);
    expect(await api.closeAllSessions()).toBe(2);
    expect(closed).toEqual([null, null]);
    expect((await api.activeWork()).sessions).toHaveLength(0);
  });

  it("local_shells_pick_a_listed_shell_only", async () => {
    const api = createMockApi({ localShells: [{ id: "a", label: "A" }, { id: "b", label: "B" }] });
    expect(await api.localShells()).toEqual([{ id: "a", label: "A" }, { id: "b", label: "B" }]);
    const handlers = { onOutput: () => {}, onClosed: () => {} };
    const size = { cols: 80, rows: 24 };
    await api.openSession({ kind: "local", shellId: "b" }, size, handlers);
    await api.openSession({ kind: "local", shellId: null }, size, handlers);
    expect(api.control.openedLocalShells()).toEqual(["b", null]);
    const gone = await apiError(api.openSession({ kind: "local", shellId: "zz" }, size, handlers));
    expect(gone).toEqual({ code: "shell", message: "That shell is no longer available.", refs: [] });
  });

  it("forwards_and_transfers_emit_events", async () => {
    const api = createMockApi();
    await api.vaultCreate("pw");
    const host = await api.upsertEntity("host", { label: "h", address: "a" });
    const rule = await api.upsertEntity("forward_rule", {
      hostId: host.id,
      label: "r",
      kind: "dynamic",
      bindAddr: "127.0.0.1",
      bindPort: 1080,
      destHost: null,
      destPort: null,
      autostart: false,
    });
    const forwardEvents: string[] = [];
    await api.onForward((e) => forwardEvents.push(e.status.state));
    expect(await api.forwardStart(rule.id)).toEqual({
      state: "active",
      detail: { bound: "127.0.0.1:1080" },
    });
    await api.forwardStop(rule.id);
    expect(forwardEvents).toEqual(["starting", "active", "stopped"]);

    const transfers: string[] = [];
    await api.onTransfer((e) => transfers.push(e.event.kind));
    await api.sftpOpen(host.id);
    const { jobId } = await api.transferEnqueue(host.id, {
      direction: "upload",
      local: "/tmp/a b.txt",
      remote: "/home/u/a b.txt",
      policy: "ask",
    });
    expect((await api.activeWork()).transfers.map((t) => t.jobId)).toEqual([jobId]);
    expect(await api.transferCancel(jobId)).toBe(true);
    expect(transfers).toEqual(["cancelled"]);
    expect((await api.activeWork()).transfers).toEqual([]);
  });

  it("bind_conflict_marks_only_the_second_rule_error", async () => {
    const api = createMockApi();
    await api.vaultCreate("pw");
    const host = await api.upsertEntity("host", { label: "h", address: "a" });
    const rule = (label: string, bindAddr: string, bindPort: number) =>
      api.upsertEntity("forward_rule", {
        hostId: host.id,
        label,
        kind: "local",
        bindAddr,
        bindPort,
        destHost: "d",
        destPort: 1,
      });
    const [a, b, other] = [
      await rule("a", "127.0.0.1", 8000),
      await rule("b", "127.0.0.1", 8000),
      await rule("c", "127.0.0.1", 8001),
    ];
    const seen: [string, string][] = [];
    await api.onForward((e) => seen.push([e.ruleId, e.status.state]));

    expect((await api.forwardStart(a.id)).state).toBe("active");
    expect(await api.forwardStart(b.id)).toEqual({ state: "error", detail: "address in use" });
    expect((await api.forwardStart(other.id)).state).toBe("active");
    // The failed rule did not touch the others; stopping the first frees the address.
    const byRule = Object.fromEntries((await api.forwardStatuses()).map((s) => [s.ruleId, s.status.state]));
    expect(byRule).toEqual({ [a.id]: "active", [b.id]: "error", [other.id]: "active" });
    await api.forwardStop(a.id);
    expect((await api.forwardStart(b.id)).state).toBe("active");
    expect(seen.filter(([id]) => id === other.id)).toEqual([[other.id, "starting"], [other.id, "active"]]);
  });

  it("quick_unlock_and_file_picker_hooks", async () => {
    const api = createMockApi();
    await api.vaultCreate("pw");
    await api.vaultEnableQuickUnlock();
    api.control.failQuickUnlock("unlock", "denied");
    await api.vaultLock();
    expect(await apiError(api.vaultQuickUnlock())).toMatchObject({ code: "keychain", message: "denied" });
    await api.vaultQuickUnlock(); // the failure was for one attempt
    expect((await api.vaultStatus()).state).toBe("unlocked");

    expect(await api.pickFile({ title: "x" })).toBeNull();
    api.control.setPick("/a/b");
    expect(await api.pickFile()).toBe("/a/b");
    expect(api.control.pickCalls()).toEqual([{ title: "x" }, {}]);
  });

  it("scan_qr_hook", async () => {
    const api = createMockApi();
    expect(await api.scanQr()).toBeNull(); // cancelled or refused by default
    api.control.setScan("tether1-link-x");
    expect(await api.scanQr()).toBe("tether1-link-x");
    await api.cancelScan();
    expect(api.control.scanCalls()).toBe(2);
  });

  it("clipboard_hook", async () => {
    const api = createMockApi();
    expect(await api.readClipboard()).toBeNull(); // empty by default
    expect(await api.writeClipboard("ls -la")).toBe(true);
    expect(api.control.clipboard()).toBe("ls -la");
    expect(await api.readClipboard()).toBe("ls -la");
    api.control.setClipboard("");
    expect(await api.readClipboard()).toBeNull();
  });
});

describe("sync, password change and conflicts (mock)", () => {
  const INVITE = "tether1-invite-demo";
  const LINK = "tether1-link-demo";

  it("parse_code_names_the_server_and_its_fingerprint", async () => {
    const api = createMockApi();
    expect(await api.sync.parseCode(INVITE)).toEqual({
      kind: "invite",
      server: MOCK_SERVER,
      fingerprint: MOCK_FINGERPRINT,
    });
    expect((await api.sync.parseCode(LINK)).kind).toBe("link");
    // The server's own format: `sha256:` and 64 lowercase hex digits, no spaces.
    expect(MOCK_FINGERPRINT).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect((await apiError(api.sync.parseCode("nonsense"))).code).toBe("invalidCode");
  });

  it("short_code_needs_check_server_and_enables_with_the_address", async () => {
    const api = createMockApi();
    await api.vaultCreate("pw");
    expect(await api.sync.parseCode("TLX-ABCD-EFGH-JKMN-PQRS")).toEqual({ kind: "short", server: "", fingerprint: null });
    expect(await api.sync.checkServer("127.0.0.1", "TLX-ABCD-EFGH-JKMN-PQRS")).toEqual({
      kind: "invite",
      server: "https://127.0.0.1:8443",
      fingerprint: MOCK_FINGERPRINT,
    });
    expect((await apiError(api.sync.checkServer("a b", "TLX-ABCD-EFGH-JKMN-PQRS"))).code).toBe("invalidAddress");
    api.control.failNextSync("checkServer", "serverUnverified", "m");
    expect((await apiError(api.sync.checkServer("127.0.0.1", "TLX-ABCD-EFGH-JKMN-PQRS"))).code).toBe("serverUnverified");
    await api.sync.enable("TLX-ABCD-EFGH-JKMN-PQRS", "alice", "pw", { new: true }, "127.0.0.1:9000");
    expect((await api.sync.status()).account?.server).toBe("https://127.0.0.1:9000");
    // The add-device code can name another address.
    const link = await api.sync.linkCode("nas.local");
    expect(link.server).toBe("https://nas.local:8443");
    expect((await apiError(api.sync.linkCode("a b"))).code).toBe("invalidAddress");
  });

  it("enable_status_devices_link_code_and_turn_off", async () => {
    const api = createMockApi();
    const { recoveryKey } = await api.vaultCreate("pw");
    expect(await api.sync.status()).toEqual({ account: null, status: { state: "off" } });
    for (const call of [api.sync.now(), api.sync.devices(), api.sync.linkCode()]) {
      expect((await apiError(call)).code).toBe("syncOff");
    }

    const seen: SyncStatus[] = [];
    await api.sync.onStatus((e) => seen.push(e));
    expect(
      (await apiError(api.sync.enable(INVITE, "alice", "nope", { existing: recoveryKey }))).code,
    ).toBe("wrongPassword");
    expect(
      (await apiError(api.sync.enable(INVITE, "alice", "pw", { existing: "wrong" }))).code,
    ).toBe("wrongRecoveryKey");
    expect((await apiError(api.sync.enable(LINK, "alice", "pw", { new: true }))).code).toBe(
      "invalidCode",
    );
    const enabled = await api.sync.enable(INVITE, " alice ", "pw", { existing: recoveryKey });
    expect(enabled).toEqual({ newRecoveryKey: null });
    expect((await apiError(api.sync.enable(INVITE, "alice", "pw", { new: true }))).code).toBe(
      "syncOn",
    );
    const status = await api.sync.status();
    expect(status.account).toMatchObject({ server: MOCK_SERVER, username: "alice" });
    expect(status.status.state).toBe("idle");
    expect(seen.map((e) => e.status.state)).toEqual(["syncing", "idle"]);

    // Devices: this one marked; rename; another device; revoke.
    api.control.addDevice({ id: "d-2", name: "Work laptop" });
    const devices = await api.sync.devices();
    expect(devices.map((d) => [d.id, d.current])).toEqual([
      [MOCK_DEVICE_ID, true],
      ["d-2", false],
    ]);
    await api.sync.renameDevice("d-2", "Laptop");
    expect((await api.sync.devices())[1].name).toBe("Laptop");
    await api.sync.revokeDevice("d-2");
    expect((await api.sync.devices()).map((d) => d.id)).toEqual([MOCK_DEVICE_ID]);
    expect((await apiError(api.sync.revokeDevice("d-2"))).code).toBe("notFound");

    // An add-device code that the mock's own join accepts.
    const link = await api.sync.linkCode();
    expect((await api.sync.parseCode(link.code)).kind).toBe("link");
    expect(link.expiresAt).toBeGreaterThan(Date.now());

    // Sync now runs a cycle; the control plays the engine's other states.
    seen.length = 0;
    await api.sync.now();
    expect(api.control.syncNowCalls()).toBe(1);
    expect(seen.map((e) => e.status.state)).toEqual(["syncing", "idle"]);
    api.control.setSyncStatus({ state: "error", kind: "certificateChanged", message: "server certificate changed" });
    expect((await api.sync.status()).status).toMatchObject({ state: "error", kind: "certificateChanged" });

    await api.sync.turnOff();
    expect(await api.sync.status()).toEqual({ account: null, status: { state: "off" } });
    expect((await apiError(api.sync.devices())).code).toBe("syncOff");
    expect((await api.vaultStatus()).state).toBe("unlocked");
  });

  it("new_recovery_key_replaces_the_old_one", async () => {
    const api = createMockApi();
    const { recoveryKey } = await api.vaultCreate("pw");
    const { newRecoveryKey } = await api.sync.enable(INVITE, "alice", "pw", { new: true });
    expect(newRecoveryKey).toBeTruthy();
    expect(newRecoveryKey).not.toBe(recoveryKey);
    await api.vaultLock();
    expect((await apiError(api.vaultUnlockRecovery(recoveryKey, "x"))).code).toBe("wrongRecoveryKey");
    await api.vaultUnlockRecovery(newRecoveryKey!, "pw2");
  });

  it("join_a_synced_vault_and_replace_an_existing_one", async () => {
    const first = createMockApi();
    const { recoveryKey } = await first.vaultCreate("pw");
    await first.sync.enable(INVITE, "alice", "pw", { existing: recoveryKey });
    const { code } = await first.sync.linkCode();

    const second = createMockApi();
    second.control.setServerAccount({ username: "alice", password: "pw", recoveryKey });
    expect((await apiError(second.sync.join(INVITE, "alice", "pw", false))).code).toBe("invalidCode");
    expect((await apiError(second.sync.join(code, "alice", "bad", false))).code).toBe("wrongPassword");
    expect((await apiError(second.sync.join(code, "bob", "pw", false))).code).toBe("wrongPassword");
    expect(await second.sync.join(code, "alice", "pw", false)).toEqual({ backup: null, reconnected: false });
    expect((await second.vaultStatus()).state).toBe("unlocked");
    expect((await second.sync.status()).account?.username).toBe("alice");

    // A device with a vault of its own is not replaced silently.
    const third = createMockApi();
    third.control.setServerAccount({ username: "alice", password: "pw", recoveryKey });
    await third.vaultCreate("other");
    await third.upsertEntity("host", { label: "mine", address: "h" });
    expect((await apiError(third.sync.join(code, "alice", "pw", false))).code).toBe("vaultExists");
    const joined = await third.sync.join(code, "alice", "pw", true);
    expect(joined.backup).toContain("before-join");
    expect(await third.listEntities("host")).toEqual([]);
  });

  it("a_failed_enable_carries_the_new_recovery_key_once_the_server_has_it", async () => {
    const api = createMockApi();
    await api.vaultCreate("pw");
    const early = await apiError(api.sync.enable(INVITE, "alice", "nope", { new: true }));
    expect(early.code).toBe("wrongPassword");
    expect(recoveryKeyOf(early)).toBeNull();
    api.control.failNextEnableAfterRegister("storage", "could not access stored credentials: x");
    const late = await apiError(api.sync.enable(INVITE, "alice", "pw", { new: true }));
    expect(late.code).toBe("storage");
    const key = recoveryKeyOf(late);
    expect(key).toMatch(/^MOCK-/);
    // It is the vault's key now.
    await api.vaultLock();
    await api.vaultUnlockRecovery(key!, "new-pw");
  });

  it("rejoining_this_devices_own_vault_keeps_its_records", async () => {
    const api = createMockApi({ lockedVault: { password: "pw", quickUnlock: false }, sync: {} });
    await api.vaultUnlock("pw");
    await api.sync.turnOff();
    await api.upsertEntity("host", { label: "while off", address: "h" });
    expect(await api.sync.join("tether1-link-again", "alice", "pw", false)).toEqual({ backup: null, reconnected: true });
    expect((await api.listEntities("host")).map((h) => (h as { label: string }).label)).toEqual(["while off"]);
    expect((await api.sync.status()).account?.username).toBe("alice");
  });

  it("recover_on_a_new_device_sets_the_password_then_asks_for_a_code", async () => {
    const api = createMockApi();
    api.control.setServerAccount({ username: "alice", password: "old", recoveryKey: "KEY-1" });
    expect((await apiError(api.sync.recover(INVITE, "alice", "wrong", "new"))).code).toBe(
      "wrongRecoveryKey",
    );
    expect(await api.sync.recover(INVITE, "alice", "KEY-1", "new")).toEqual({
      needsDeviceCode: true,
      server: MOCK_SERVER,
    });
    expect((await api.vaultStatus()).state).toBe("uninitialized");
    expect((await apiError(api.sync.join(LINK, "alice", "old", false))).code).toBe("wrongPassword");
    await api.sync.join(LINK, "alice", "new", false);
    expect((await api.vaultStatus()).state).toBe("unlocked");
  });

  it("change_password_works_with_sync_off_and_on", async () => {
    const api = createMockApi();
    await api.vaultCreate("pw");
    expect((await apiError(api.vault.changePassword("bad", "next"))).code).toBe("wrongPassword");
    await api.vault.changePassword("pw", "next");
    expect((await api.vaultStatus()).state).toBe("unlocked");
    await api.vaultLock();
    expect((await apiError(api.vaultUnlock("pw"))).code).toBe("wrongPassword");
    await api.vaultUnlock("next");

    const synced = createMockApi({ lockedVault: { password: "pw", quickUnlock: false }, sync: {} });
    await synced.vaultUnlock("pw");
    // Offline, the change cannot reach the server and nothing changes.
    synced.control.setSyncStatus({ state: "offline", reason: "connection refused" });
    expect((await apiError(synced.vault.changePassword("pw", "next"))).code).toBe("unreachable");
    synced.control.setSyncStatus({ state: "idle", lastSync: 1 });
    await synced.vault.changePassword("pw", "next");
    await synced.vaultLock();
    await synced.vaultUnlock("next");
  });

  it("password_changed_elsewhere_locks_the_vault_with_a_reason", async () => {
    const api = createMockApi({ lockedVault: { password: "pw", quickUnlock: false }, sync: {} });
    await api.vaultUnlock("pw");
    const locks: VaultLocked[] = [];
    await api.onVaultLocked((e) => locks.push(e));
    api.control.changePasswordElsewhere("elsewhere");
    expect(locks).toEqual([{ reason: "passwordChanged" }]);
    expect((await api.vaultStatus()).state).toBe("locked");
    expect((await apiError(api.vaultUnlock("pw"))).code).toBe("wrongPassword");
    await api.vaultUnlock("elsewhere");
    await api.vaultLock();
    expect(locks[locks.length - 1]).toEqual({ reason: "manual" });
  });

  it("a_stale_device_opens_with_the_new_password_after_a_refresh", async () => {
    const api = createMockApi({ lockedVault: { password: "pw", quickUnlock: false }, sync: {} });
    const locks: VaultLocked[] = [];
    await api.onVaultLocked((e) => locks.push(e));
    api.control.changePasswordElsewhere("elsewhere", { stale: true });
    expect(locks).toEqual([]);
    // Offline it cannot refresh.
    api.control.setSyncStatus({ state: "offline", reason: "r" });
    expect((await apiError(api.vaultUnlock("elsewhere"))).code).toBe("wrongPassword");
    api.control.setSyncStatus({ state: "idle", lastSync: 1 });
    await api.vaultUnlock("elsewhere");
    expect((await api.vaultStatus()).state).toBe("unlocked");
  });

  // Rust's `sync_failures_use_the_codes_the_mock_documents` runs the same cases against the real
  // commands and compares with the same file, so the mock cannot drift from the backend.
  it("mock_rejects_with_the_codes_the_backend_does", async () => {
    const got: Record<string, string> = {};
    const code = async (p: Promise<unknown>) => (await apiError(p)).code;

    const off = createMockApi();
    got.nowWhileOff = await code(off.sync.now());
    got.devicesWhileOff = await code(off.sync.devices());
    got.signInWhileOff = await code(off.sync.signIn("pw"));
    got.parseGarbageCode = await code(off.sync.parseCode("not a code"));
    got.checkServerBadAddress = await code(off.sync.checkServer("bad address/", "TLX-ABCD-EFGH-JKMN-PQRS"));
    got.enableBeforeCreate = await code(off.sync.enable(INVITE, "alice", "pw", { new: true }));
    await off.vaultCreate("pw");
    await off.vaultLock();
    got.enableWhileLocked = await code(off.sync.enable(INVITE, "alice", "pw", { new: true }));
    await off.vaultUnlock("pw");
    got.enableWithAddDeviceCode = await code(off.sync.enable(LINK, "alice", "pw", { new: true }));
    got.joinOverExistingVault = await code(off.sync.join(LINK, "alice", "pw", false));

    const synced = createMockApi({ lockedVault: { password: "pw", quickUnlock: false }, sync: {} });
    await synced.vaultUnlock("pw");
    got.linkCodeBadAddress = await code(synced.sync.linkCode("a b"));
    got.renameUnknownDevice = await code(synced.sync.renameDevice("nope", "x"));
    got.revokeUnknownDevice = await code(synced.sync.revokeDevice("nope"));

    expect(got).toEqual(syncErrorCodes);
  });

  it("failed_sync_commands_reject_once_with_the_given_error", async () => {
    const api = createMockApi();
    await api.vaultCreate("pw");
    api.control.failNextSync("enable", "certificateChanged", "server certificate changed");
    expect(await apiError(api.sync.enable(INVITE, "alice", "pw", { new: true }))).toMatchObject({
      code: "certificateChanged",
      message: "server certificate changed",
    });
    await api.sync.enable(INVITE, "alice", "pw", { new: true });
  });

  it("conflicts_mask_secrets_and_reveal_them_one_field_at_a_time", async () => {
    const api = createMockApi();
    await api.vaultCreate("pw");
    expect(await api.conflicts.list()).toEqual([]);
    const conflict = api.control.addConflict({
      kind: "identity",
      current: { id: "i-1", label: "ops", username: "root", password: "CANARY-cur", key_id: null },
      other: { id: "i-1", label: "ops-b", username: "root", password: "CANARY-oth", key_id: null },
    });
    expect(conflict.changedFields).toEqual(["label", "password"]);
    const [listed] = await api.conflicts.list();
    expect(listed.current).toMatchObject({ label: "ops", password: { hasSecret: true } });
    expect(listed.other).toMatchObject({ label: "ops-b", password: { hasSecret: true } });
    expect(JSON.stringify(await api.conflicts.list())).not.toContain("CANARY");

    expect(await api.conflicts.reveal(conflict.id, "other", "password")).toBe("CANARY-oth");
    expect(await api.conflicts.reveal(conflict.id, "current", "password")).toBe("CANARY-cur");
    expect((await apiError(api.conflicts.reveal(conflict.id, "other", "label"))).code).toBe("invalid");

    expect(
      (await apiError(api.conflicts.resolve(conflict.id, { fields: ["nope"] }))).code,
    ).toBe("invalid");
    await api.conflicts.resolve(conflict.id, { fields: ["password"] });
    expect(await api.conflicts.list()).toEqual([]);
    expect(api.control.resolvedConflicts()).toEqual([
      { id: conflict.id, resolution: { fields: ["password"] } },
    ]);
    expect((await apiError(api.conflicts.resolve(conflict.id, { keepCurrent: true }))).code).toBe(
      "notFound",
    );

    // A deletion is a side with no record.
    const gone = api.control.addConflict({
      kind: "host",
      current: { id: "h-1", label: "web" },
      other: null,
    });
    expect(gone.changedFields).toEqual(["id", "label"]);
    expect((await apiError(api.conflicts.reveal(gone.id, "other", "password"))).code).toBe("invalid");
  });
});
