import { describe, expect, it } from "vitest";
import type { Group, Host, Identity } from "../api/types";
import { badgeIndex } from "../hosts/browse";
import { hostHeader } from "./useHostHeader";

const NO_SETTINGS = {
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
const host = (over: Partial<Host["settings"]> = {}): Host => ({
  id: "h1",
  groupId: null,
  label: "Web One",
  address: "web1.example",
  tagIds: [],
  settings: { ...NO_SETTINGS, ...over },
  forwardRuleIds: [],
});
const identity: Identity = {
  id: "i1",
  label: "deploy",
  username: "deploy",
  hasPassword: true,
  keyId: null,
  owner: null,
};

describe("hostHeader", () => {
  it("shows_the_login_address_and_port_with_the_hosts_page_badge", () => {
    const out = hostHeader(host({ identityId: "i1", port: 2200 }), [], [identity]);
    expect(out).toEqual({
      header: {
        label: "Web One",
        address: "deploy@web1.example:2200",
        badge: { text: "WO", colorIndex: badgeIndex("h1") },
      },
      host: "web1.example",
      port: 2200,
    });
  });

  it("always_names_the_port_and_drops_the_user_when_there_is_none", () => {
    const groups: Group[] = [];
    expect(hostHeader(host(), groups, []).header.address).toBe("web1.example:22");
  });
});
