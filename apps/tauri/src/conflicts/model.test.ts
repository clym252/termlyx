import { describe, expect, it } from "vitest";
import type { Conflict } from "../api/types";
import { conflictCountLabel, conflictTitle, discardedSecrets, fieldRows, joinLabels, replacedSecrets } from "./model";

const names: Record<string, string> = {
  g1: "Production",
  g2: "Staging",
  t1: "db",
  t2: "web",
  i1: "deploy",
  k1: "laptop key",
};
const name = (_kind: string, id: string) => names[id];

const find = (rows: ReturnType<typeof fieldRows>, field: string) => {
  const row = rows.find((r) => r.field === field);
  if (!row) throw new Error(`no row ${field}`);
  return row;
};

describe("fieldRows", () => {
  it("field_rows_flags_changes_and_masks_secrets", () => {
    const current = { id: "x", label: "prod-db", username: "root", password: { hasSecret: true } };
    const other = { id: "x", label: "prod-db", username: "admin", password: { hasSecret: true } };
    const rows = fieldRows(current, other, ["password", "username"], { kind: "identity" });

    // One row per top-level field; the id is not a field a user can pick.
    expect(rows.map((r) => r.field)).toEqual(["label", "username", "password"]);
    expect(rows.map((r) => r.changed)).toEqual([false, true, true]);
    expect(find(rows, "label").label).toBe("Label");
    expect(find(rows, "username").current).toEqual({ kind: "value", lines: ["root"] });
    expect(find(rows, "username").other).toEqual({ kind: "value", lines: ["admin"] });
    // Secrets are markers, never values; the page shows them as "••••" with a Reveal button.
    const password = find(rows, "password");
    expect(password.secret).toBe(true);
    expect(password.current).toEqual({ kind: "secret" });
    expect(password.other).toEqual({ kind: "secret" });
    expect(JSON.stringify(rows)).not.toContain("hasSecret");
  });

  it("shows_referenced_records_by_name_and_missing_ones_as_missing", () => {
    const current = { id: "h", label: "a", group_id: "g1", tag_ids: ["t1", "t2"], settings: null };
    const other = { id: "h", label: "a", group_id: "gone", tag_ids: [], settings: null };
    const rows = fieldRows(current, other, ["group_id", "tag_ids"], { kind: "host", name });
    expect(find(rows, "group_id").label).toBe("Group");
    expect(find(rows, "group_id").current).toEqual({ kind: "value", lines: ["Production"] });
    expect(find(rows, "group_id").other).toEqual({ kind: "value", lines: ["(missing)"] });
    expect(find(rows, "tag_ids").current).toEqual({ kind: "value", lines: ["db, web"] });
    expect(find(rows, "tag_ids").other).toEqual({ kind: "absent" });
    expect(find(rows, "settings").current).toEqual({ kind: "absent" });
  });

  it("flattens_changed_settings_into_labelled_lines", () => {
    const current = { id: "h", settings: { port: 22, keepalive_secs: 30, identity_id: "i1", charset: "utf-8" } };
    const other = { id: "h", settings: { port: 2222, keepalive_secs: 30, identity_id: "gone", font_size: 14 } };
    const rows = fieldRows(current, other, ["settings"], { kind: "host", name });
    const settings = find(rows, "settings");
    expect(settings.changed).toBe(true);
    // Only the sub-fields that differ, the same ones on both sides.
    expect(settings.current).toEqual({
      kind: "value",
      lines: ["Port: 22", "Login: deploy", "Character set: utf-8", "Font size: —"],
    });
    expect(settings.other).toEqual({
      kind: "value",
      lines: ["Port: 2222", "Login: (missing)", "Character set: —", "Font size: 14"],
    });
  });

  it("shows_every_set_setting_when_settings_did_not_change", () => {
    const same = { id: "h", settings: { port: 22, keepalive_secs: null } };
    const rows = fieldRows(same, same, [], { kind: "host" });
    expect(find(rows, "settings").changed).toBe(false);
    expect(find(rows, "settings").current).toEqual({ kind: "value", lines: ["Port: 22"] });
  });

  it("a_deleted_side_has_only_absent_cells", () => {
    const current = { id: "k", label: "laptop key", algorithm: "ed25519", private_openssh: { hasSecret: true }, passphrase: null };
    const rows = fieldRows(current, null, ["algorithm", "label", "passphrase", "private_openssh"], { kind: "ssh_key" });
    expect(rows.map((r) => r.field)).toEqual(["label", "algorithm", "private_openssh", "passphrase"]);
    for (const r of rows) expect(r.other).toEqual({ kind: "absent" });
    expect(find(rows, "private_openssh").current).toEqual({ kind: "secret" });
    // An unset secret is not a secret to reveal.
    expect(find(rows, "passphrase").secret).toBe(false);
    expect(find(rows, "passphrase").current).toEqual({ kind: "absent" });
  });

  it("falls_back_to_the_raw_name_for_unknown_fields_and_kinds", () => {
    const rows = fieldRows({ id: "x", from_the_future: 3, label: "a" }, { id: "x", from_the_future: 4, label: "a" }, ["from_the_future"]);
    // Without a kind the fields keep the record's own order.
    expect(rows.map((r) => r.field)).toEqual(["from_the_future", "label"]);
    expect(find(rows, "from_the_future").label).toBe("from_the_future");
    expect(find(rows, "from_the_future").current).toEqual({ kind: "value", lines: ["3"] });
  });

  it("formats_booleans_dates_and_environment_pairs", () => {
    const rule = fieldRows({ id: "r", autostart: true }, { id: "r", autostart: false }, ["autostart"], { kind: "forward_rule" });
    expect(find(rule, "autostart").label).toBe("Start automatically");
    expect(find(rule, "autostart").current).toEqual({ kind: "value", lines: ["Yes"] });
    expect(find(rule, "autostart").other).toEqual({ kind: "value", lines: ["No"] });

    const env = fieldRows(
      { id: "h", settings: { env: [["A", "1"]] } },
      { id: "h", settings: { env: [["A", "1"], ["B", "2"]] } },
      ["settings"],
      { kind: "host" },
    );
    expect(find(env, "settings").other).toEqual({ kind: "value", lines: ["Environment: A=1, B=2"] });
  });
});

describe("conflictTitle", () => {
  const conflict = (kind: Conflict["kind"], current: Record<string, unknown> | null, other: Record<string, unknown> | null = null): Conflict => ({
    id: "c",
    recordId: "r",
    kind,
    current,
    other,
    changedFields: [],
    detectedAt: 0,
  });

  it("names_the_kind_and_the_record", () => {
    expect(conflictTitle(conflict("host", { label: "prod-db" }), name)).toBe("Host · prod-db");
    expect(conflictTitle(conflict("ssh_key", null, { label: "laptop key" }), name)).toBe("SSH key · laptop key");
    expect(conflictTitle(conflict("known_host", { host_pattern: "example.com", port: 2200 }), name)).toBe(
      "Known host · example.com:2200",
    );
    expect(conflictTitle(conflict("group", { name: "Production" }), name)).toBe("Group · Production");
  });

  it("titles_the_hidden_own_login_of_a_host", () => {
    const own = conflict("identity", { label: "x", owner: "g1" });
    expect(conflictTitle(own, name)).toBe("Login of Production");
    expect(conflictTitle(conflict("identity", { label: "x", owner: "nobody" }), name)).toBe("Login of (missing)");
    expect(conflictTitle(conflict("identity", { label: "deploy", owner: null }), name)).toBe("Identity · deploy");
  });

  it("copes_with_a_record_without_a_label", () => {
    expect(conflictTitle(conflict("tag", null, null), name)).toBe("Tag");
  });
});

describe("conflictCountLabel", () => {
  it("pluralises", () => {
    expect(conflictCountLabel(1)).toBe("1 conflict");
    expect(conflictCountLabel(3)).toBe("3 conflicts");
  });
});

describe("discardedSecrets", () => {
  const key: Conflict = {
    id: "c",
    recordId: "k",
    kind: "ssh_key",
    current: { id: "k", label: "a", private_openssh: { hasSecret: true }, passphrase: { hasSecret: true } },
    other: { id: "k", label: "b", private_openssh: { hasSecret: true }, passphrase: { hasSecret: true } },
    changedFields: ["label", "passphrase", "private_openssh"],
    detectedAt: 0,
  };

  it("names_the_changed_secrets_of_the_discarded_side", () => {
    expect(discardedSecrets(key, "other")).toEqual(["private key", "passphrase"]);
    expect(discardedSecrets(key, "current")).toEqual(["private key", "passphrase"]);
  });

  it("leaves_out_unchanged_secrets_and_picked_fields", () => {
    const same = { ...key, changedFields: ["label", "passphrase"] };
    expect(discardedSecrets(same, "other")).toEqual(["passphrase"]);
    expect(discardedSecrets(key, "other", new Set(["private_openssh"]))).toEqual(["passphrase"]);
  });

  it("a_deleted_side_discards_nothing", () => {
    expect(discardedSecrets({ ...key, other: null }, "other")).toEqual([]);
    expect(discardedSecrets({ ...key, other: null }, "current")).toEqual(["private key", "passphrase"]);
  });

  it("names_the_current_secrets_a_pick_overwrites", () => {
    expect(replacedSecrets(key, new Set(["private_openssh", "label"]))).toEqual(["private key"]);
    expect(replacedSecrets(key, new Set())).toEqual([]);
    // Nothing is overwritten where the current version has no secret.
    const unset = { ...key, current: { ...key.current, passphrase: null } };
    expect(replacedSecrets(unset, new Set(["passphrase"]))).toEqual([]);
  });

  it("joins_labels", () => {
    expect(joinLabels(["password"])).toBe("password");
    expect(joinLabels(["private key", "passphrase"])).toBe("private key and passphrase");
  });
});
