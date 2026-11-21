import { describe, expect, it } from "vitest";
import type { HostKeyInfo, PromptEvent } from "../api/types";
import { hostKeyPending } from "./pending";

const key: HostKeyInfo = { algorithm: "ssh-ed25519", sha256Fingerprint: "SHA256:x", openssh: "ssh-ed25519 AAAA" };
const unknown = (host: string, port: number): PromptEvent => ({
  id: `${host}:${port}`,
  prompt: { kind: "hostKeyUnknown", host, port, key },
});

describe("hostKeyPending", () => {
  it("matches_a_waiting_host_key_prompt_for_that_host_and_port", () => {
    const queue = [unknown("Web1.example", 22)];
    expect(hostKeyPending(queue, "web1.example", 22)).toBe(true);
    expect(hostKeyPending(queue, "web1.example", 2200)).toBe(false);
    expect(hostKeyPending(queue, "db1.example", 22)).toBe(false);
  });

  it("covers_a_changed_key_too_and_nothing_else", () => {
    const changed: PromptEvent = {
      id: "c",
      prompt: { kind: "hostKeyChanged", host: "web1.example", port: 22, old: key, new: key },
    };
    expect(hostKeyPending([changed], "web1.example", 22)).toBe(true);
    const password: PromptEvent = {
      id: "p",
      prompt: { kind: "password", user: "deploy", host: "web1.example", saveTo: null },
    };
    expect(hostKeyPending([password], "web1.example", 22)).toBe(false);
  });

  it("is_false_without_a_host", () => {
    expect(hostKeyPending([unknown("web1.example", 22)], null, 22)).toBe(false);
  });
});
