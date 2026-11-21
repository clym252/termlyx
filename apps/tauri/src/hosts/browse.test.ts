import { describe, expect, it } from "vitest";
import type { ConnSettings, Group, Host, Identity } from "../api/types";
import {
  badgeIndex,
  breadcrumb,
  childGroups,
  effectiveLogin,
  filterActive,
  filterHosts,
  hostSubtitle,
  hostsAt,
  initials,
  resolveCurrentGroup,
} from "./browse";

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

const host = (
  id: string,
  label: string,
  groupId: string | null,
  address = "h",
  tagIds: string[] = [],
  settings: Partial<ConnSettings> = {},
): Host => ({ id, label, address, groupId, tagIds, settings: { ...none, ...settings }, forwardRuleIds: [] });

const identity = (id: string, username: string): Identity => ({
  id,
  label: id,
  username,
  hasPassword: false,
  keyId: null,
  owner: null,
});

const ids = (list: { id: string }[]) => list.map((x) => x.id);

describe("childGroups", () => {
  it("returns top-level groups: null parent, missing parent, and every member of a cycle", () => {
    const groups = [
      group("root", "Root", null),
      group("orphan", "Orphan", "gone"),
      group("c1", "Cycle one", "c2"),
      group("c2", "Cycle two", "c1"),
      group("child", "Child", "root"),
    ];
    expect(ids(childGroups(groups, null))).toEqual(["c1", "c2", "orphan", "root"]);
  });

  it("returns direct children only, sorted case-insensitively", () => {
    const groups = [
      group("p", "P", null),
      group("b", "beta", "p"),
      group("a", "Alpha", "p"),
      group("c", "Charlie", "p"),
      group("deep", "Aaa", "a"),
    ];
    expect(ids(childGroups(groups, "p"))).toEqual(["a", "b", "c"]);
    expect(childGroups(groups, "c")).toEqual([]);
  });
});

describe("hostsAt", () => {
  const hosts = [host("1", "zeta", null), host("2", "alpha", "g"), host("3", "gamma", "sub"), host("4", "Beta", null)];

  it("null means every host, grouped or not, sorted", () => {
    expect(ids(hostsAt(hosts, null))).toEqual(["2", "4", "3", "1"]);
  });

  it("a group id means only that group's own hosts, not its subgroups'", () => {
    expect(ids(hostsAt(hosts, "g"))).toEqual(["2"]);
  });

  it("lists a host whose group is missing (sync can leave one) at the top level only", () => {
    const dangling = [...hosts, host("5", "orphan", "gone")];
    expect(ids(hostsAt(dangling, null))).toEqual(["2", "4", "3", "5", "1"]);
    expect(ids(hostsAt(dangling, "g"))).toEqual(["2"]);
  });
});

describe("breadcrumb", () => {
  it("lists a 3-deep group root first", () => {
    const groups = [group("c", "C", "b"), group("a", "A", null), group("b", "B", "a")];
    expect(ids(breadcrumb(groups, "c"))).toEqual(["a", "b", "c"]);
    expect(breadcrumb(groups, null)).toEqual([]);
  });

  it("terminates on a cycle", () => {
    const groups = [group("a", "A", "b"), group("b", "B", "a")];
    expect(ids(breadcrumb(groups, "a")).sort()).toEqual(["a", "b"]);
  });
});

describe("resolveCurrentGroup", () => {
  const groups = [group("a", "A", null), group("b", "B", "a")];

  it("keeps a remembered group that still exists", () => {
    expect(resolveCurrentGroup(groups, "b", ["a", "b"])).toBe("b");
  });

  it("falls back to the deepest surviving ancestor", () => {
    expect(resolveCurrentGroup(groups, "gone", ["a", "gone"])).toBe("a");
    expect(resolveCurrentGroup(groups, "gone", ["a", "b", "gone"])).toBe("b");
  });

  it("falls back to the top level when nothing survives", () => {
    expect(resolveCurrentGroup(groups, "gone", ["x", "gone"])).toBeNull();
    expect(resolveCurrentGroup([], "gone", [])).toBeNull();
  });
});

describe("login and subtitle", () => {
  const identities = [identity("deploy-id", "deploy"), identity("group-id", "grp")];

  it("shows user@address from the host's identity", () => {
    const h = host("h", "web", null, "10.0.0.5", [], { identityId: "deploy-id" });
    expect(effectiveLogin(h, [], identities)).toEqual({ username: "deploy", port: 22 });
    expect(hostSubtitle(h, [], identities)).toBe("deploy@10.0.0.5");
  });

  it("appends a group-inherited non-default port", () => {
    const groups = [group("g", "G", null, { port: 2222 })];
    const h = host("h", "web", "g", "10.0.0.5", [], { identityId: "deploy-id" });
    expect(effectiveLogin(h, groups, identities).port).toBe(2222);
    expect(hostSubtitle(h, groups, identities)).toBe("deploy@10.0.0.5:2222");
  });

  it("inherits the identity from the group and lets the host override the port", () => {
    const groups = [group("g", "G", null, { identityId: "group-id", port: 2222 })];
    const h = host("h", "web", "g", "10.0.0.5", [], { port: 22 });
    expect(effectiveLogin(h, groups, identities)).toEqual({ username: "grp", port: 22 });
    expect(hostSubtitle(h, groups, identities)).toBe("grp@10.0.0.5");
  });

  it("shows only the address without an identity, or with an unknown one", () => {
    expect(hostSubtitle(host("h", "web", null, "10.0.0.5"), [], identities)).toBe("10.0.0.5");
    const dangling = host("h", "web", null, "10.0.0.5", [], { identityId: "nope" });
    expect(effectiveLogin(dangling, [], identities).username).toBeNull();
    expect(hostSubtitle(dangling, [], identities)).toBe("10.0.0.5");
  });

  it("still shows a non-default port when there is no username", () => {
    expect(hostSubtitle(host("h", "web", null, "10.0.0.5", [], { port: 2200 }), [], identities)).toBe("10.0.0.5:2200");
  });
});

describe("filterActive", () => {
  it("is true for non-blank text or any tag", () => {
    expect(filterActive("", new Set())).toBe(false);
    expect(filterActive("   ", new Set())).toBe(false);
    expect(filterActive(" x ", new Set())).toBe(true);
    expect(filterActive("", new Set(["t"]))).toBe(true);
  });
});

describe("filterHosts", () => {
  const identities = [identity("deploy-id", "deploy"), identity("root-id", "root")];
  const groups = [group("g", "G", null), group("sub", "Sub", "g"), group("other", "Other", null)];
  const hosts = [
    host("1", "web", "g", "10.0.0.5", ["t1", "t2"], { identityId: "deploy-id" }),
    host("2", "db", "sub", "10.0.0.6", ["t1"], { identityId: "deploy-id" }),
    host("3", "nas", "other", "10.0.1.9", ["t1", "t2"], { identityId: "root-id" }),
    host("4", "loose", null, "192.168.1.1", []),
  ];
  const noTags = new Set<string>();

  it("matches every word across label, address and username, case-insensitively", () => {
    expect(ids(filterHosts(hosts, groups, identities, null, "DEP 10.0", noTags))).toEqual(["2", "1"]);
    expect(ids(filterHosts(hosts, groups, identities, null, "ROOT nas", noTags))).toEqual(["3"]);
    expect(filterHosts(hosts, groups, identities, null, "deploy nas", noTags)).toEqual([]);
  });

  it("returns everything, sorted, for an empty filter", () => {
    expect(ids(filterHosts(hosts, groups, identities, null, "  ", noTags))).toEqual(["2", "4", "3", "1"]);
  });

  it("limits to the scope's subtree", () => {
    expect(ids(filterHosts(hosts, groups, identities, "g", "", noTags))).toEqual(["2", "1"]);
    expect(ids(filterHosts(hosts, groups, identities, "sub", "", noTags))).toEqual(["2"]);
  });

  it("requires every selected tag", () => {
    expect(ids(filterHosts(hosts, groups, identities, null, "", new Set(["t1", "t2"])))).toEqual(["3", "1"]);
    expect(ids(filterHosts(hosts, groups, identities, "g", "", new Set(["t2"])))).toEqual(["1"]);
  });
});

describe("initials", () => {
  it.each([
    ["tether-server", "TS"],
    ["nas", "NA"],
    ["pi", "PI"],
    ["x", "X"],
    ["🚀 rocket", "🚀R"],
    ["東京 server", "東S"],
    ["---", "?"],
    ["", "?"],
    ["web.prod_eu", "WP"],
    ["🚀🚀🚀", "🚀🚀"],
  ])("%j -> %j", (label, expected) => {
    expect(initials(label)).toBe(expected);
  });
});

describe("badgeIndex", () => {
  it("is stable and within 0-7", () => {
    for (const id of ["a", "host-1", "0190c6a0-aaaa-bbbb-cccc-ddddeeeeffff", "🚀"]) {
      const i = badgeIndex(id);
      expect(i).toBe(badgeIndex(id));
      expect(Number.isInteger(i)).toBe(true);
      expect(i).toBeGreaterThanOrEqual(0);
      expect(i).toBeLessThan(8);
    }
  });
});
