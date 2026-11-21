import { describe, expect, it } from "vitest";
import { createMockApi } from "../api/mock";
import type { ConnSettings, Group, Host, Tag } from "../api/types";
import { inheritedSettings, SETTING_KEYS, sourceLabel } from "./effective";
import { matchesQuery, matchesTags } from "./filter";
import { buildTree, groupContents, groupOptions, groupPath, hostCounts, subtreeIds } from "./model";
import { draftFromSettings, settingsFromDraft } from "./settingsDraft";

const none: ConnSettings = {
  port: null,
  identityId: null,
  keepaliveSecs: null,
  termType: null,
  charset: null,
  colorScheme: null,
  fontFamily: null,
  fontSize: null,
  env: null,
};

const group = (id: string, name: string, parentId: string | null, settings: Partial<ConnSettings> = {}): Group => ({
  id,
  name,
  parentId,
  settings: { ...none, ...settings },
});

const host = (id: string, label: string, groupId: string | null, tagIds: string[] = [], address = "h"): Host => ({
  id,
  label,
  address,
  groupId,
  tagIds,
  settings: none,
  forwardRuleIds: [],
});

describe("group model", () => {
  const groups = [group("b", "Web", "a"), group("a", "Production", null), group("c", "Staging", null)];

  it("builds a sorted forest and full paths", () => {
    const tree = buildTree(groups);
    expect(tree.map((n) => n.group.name)).toEqual(["Production", "Staging"]);
    expect(tree[0].children.map((n) => n.group.name)).toEqual(["Web"]);
    expect(groupPath(groups, "b")).toBe("Production / Web");
    expect(groupPath(groups, null)).toBe("");
    expect(groupOptions(groups).map((o) => o.path)).toEqual(["Production", "Production / Web", "Staging"]);
    expect(groupOptions(groups, subtreeIds(groups, "a")).map((o) => o.path)).toEqual(["Staging"]);
  });

  it("counts what a delete would take", () => {
    const hosts = [host("1", "x", "b"), host("2", "y", "a"), host("3", "z", "c"), host("4", "w", null)];
    expect(groupContents(groups, hosts, "a")).toEqual({ subgroups: 1, hosts: 2 });
    expect(groupContents(groups, hosts, "c")).toEqual({ subgroups: 0, hosts: 1 });
  });

  it("host_counts_include_subgroups", () => {
    const groups = [group("p", "Production", null), group("w", "Web", "p"), group("l", "Lab", null)];
    const hosts = [host("1", "db", "p"), host("2", "web1", "w"), host("3", "web2", "w"), host("4", "pi", null)];
    const counts = hostCounts(groups, hosts);
    expect(counts.get("p")).toBe(3);
    expect(counts.get("w")).toBe(2);
    expect(counts.get("l")).toBe(0);
  });

  it("survives a parent cycle", () => {
    const cyclic = [group("a", "A", "b"), group("b", "B", "a")];
    expect(groupPath(cyclic, "a").length).toBeGreaterThan(0);
    expect(inheritedSettings(cyclic, "a").port.value).toBe(22);
  });
});

describe("host filters", () => {
  const tags: Tag[] = [
    { id: "t1", name: "prod", color: "#fff" },
    { id: "t2", name: "db", color: "#fff" },
  ];
  const h = host("1", "東京 ノード", null, ["t1", "t2"], "192.168.9.9");

  it("matches label, address and tag names, every word, case-insensitively", () => {
    expect(matchesQuery(h, tags, "")).toBe(true);
    expect(matchesQuery(h, tags, "東京")).toBe(true);
    expect(matchesQuery(h, tags, "192.168")).toBe(true);
    expect(matchesQuery(h, tags, "PROD")).toBe(true);
    expect(matchesQuery(h, tags, "prod 東京")).toBe(true);
    expect(matchesQuery(h, tags, "prod staging")).toBe(false);
  });

  it("requires every selected tag", () => {
    expect(matchesTags(h, new Set(["t1", "t2"]))).toBe(true);
    expect(matchesTags(h, new Set(["t1", "t3"]))).toBe(false);
    expect(matchesTags(h, new Set())).toBe(true);
  });
});

describe("settings draft", () => {
  it("round-trips settings through text", () => {
    const s: ConnSettings = {
      ...none,
      port: 2200,
      env: [
        ["LANG", "en_US.UTF-8"],
        ["A", "b=c"],
      ],
      termType: "ünï",
      charset: "ISO-8859-1", // hidden in the editors, still kept
    };
    const result = settingsFromDraft(draftFromSettings(s));
    expect(result).toEqual({ ok: true, settings: s });
  });

  it("treats empty text as inheriting and rejects bad numbers", () => {
    const draft = draftFromSettings(none);
    expect(settingsFromDraft({ ...draft, port: "" })).toEqual({ ok: true, settings: none });
    for (const bad of ["0", "70000", "22.5", "-1", "abc"]) {
      const r = settingsFromDraft({ ...draft, port: bad });
      expect(r.ok).toBe(false);
    }
    expect(settingsFromDraft({ ...draft, keepaliveSecs: "0" })).toMatchObject({ ok: true });
    expect(settingsFromDraft({ ...draft, env: "NOEQUALS" }).ok).toBe(false);
  });
});

describe("inherited settings", () => {
  it("labels the nearest source", () => {
    const groups = [group("a", "Production", null, { port: 2200 }), group("b", "Web", "a", { keepaliveSecs: 5 })];
    const eff = inheritedSettings(groups, "b");
    expect(eff.port).toEqual({ value: 2200, source: { kind: "group", groupId: "a" } });
    expect(eff.keepaliveSecs.source).toEqual({ kind: "group", groupId: "b" });
    expect(eff.termType.source).toEqual({ kind: "default" });
    expect(sourceLabel(eff.port.source, groups, "host")).toBe("from group Production");
    expect(sourceLabel({ kind: "default" }, groups, "host")).toBe("default");
    expect(sourceLabel({ kind: "host" }, groups, "group")).toBe("set on this group");
  });

  it("agrees with the backend's effective settings for a host with no own values", async () => {
    const api = createMockApi();
    await api.vaultCreate("pw");
    const prod = await api.upsertEntity("group", { name: "Production", settings: { port: 2200, fontSize: 16 } });
    const web = await api.upsertEntity("group", {
      name: "Web",
      parentId: prod.id,
      settings: { keepaliveSecs: 0, env: [["A", "1"]] },
    });
    for (const groupId of [null, prod.id, web.id]) {
      const h = await api.upsertEntity("host", { label: "h", address: "a", groupId });
      const groups = await api.listEntities("group");
      const backend = await api.effectiveSettings(h.id);
      const local = inheritedSettings(groups, groupId);
      for (const key of SETTING_KEYS) expect(local[key], key).toEqual(backend[key]);
    }
  });
});
