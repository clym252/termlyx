import { describe, expect, it } from "vitest";
import type { Group, Host, Identity } from "../api/types";
import { describeRefs } from "./refs";

const settings = {
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

describe("describeRefs", () => {
  it("names_hosts_groups_and_identities_and_keeps_unknown_ids", () => {
    const hosts: Host[] = [
      { id: "h1", groupId: null, label: "web-1", address: "a", tagIds: [], settings, forwardRuleIds: [] },
    ];
    const groups: Group[] = [{ id: "g1", parentId: null, name: "Production", settings }];
    const identities: Identity[] = [{ id: "i1", label: "ops", username: "root", hasPassword: false, keyId: null, owner: null }];
    expect(describeRefs(["h1", "g1", "i1", "gone"], { hosts, groups, identities })).toEqual([
      "Host web-1",
      "Group Production",
      "Identity ops",
      "Record gone",
    ]);
  });
});
