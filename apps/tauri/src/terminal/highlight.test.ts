import { describe, expect, it } from "vitest";
import { findIps } from "./highlight";

const hits = (s: string) => findIps(s).map((m) => s.slice(m.start, m.end));

describe("findIps", () => {
  it("finds IPv4 with port/prefix and IPv6", () => {
    expect(hits("Last login: Wed Oct 28 09:03:11 2026 from 192.168.1.174")).toEqual(["192.168.1.174"]);
    expect(hits("10.0.0.0/24 and 10.0.0.1:22")).toEqual(["10.0.0.0/24", "10.0.0.1:22"]);
    expect(hits("inet6 fe80::1%eth0/64 2001:db8::8a2e:370:7334")).toEqual(["fe80::1%eth0/64", "2001:db8::8a2e:370:7334"]);
    expect(hits("listen [::1]:8080 and ::")).toEqual(["::1"]);
    expect(hits("(127.0.0.1)")).toEqual(["127.0.0.1"]);
    expect(hits("2001:0db8:0000:0000:0000:ff00:0042:8329")).toEqual(["2001:0db8:0000:0000:0000:ff00:0042:8329"]);
  });

  it("ignores versions, times, MACs, hashes, bad octets", () => {
    for (const s of [
      "Linux 6.8.0-138-generic",
      "Ubuntu 24.04.5 LTS",
      "Wed Oct 28 09:03:11 2026",
      "ether aa:bb:cc:dd:ee:ff",
      "256.1.1.1",
      "1.2.3.4.5",
      "commit 8b50148a9c",
      "1.2.3",
      "std::vector",
      "a::b",
      "12:30",
    ]) {
      expect(hits(s), s).toEqual([]);
    }
  });
});
