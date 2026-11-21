import { describe, expect, it } from "vitest";
import { sendSnippet, snippetTextOf } from "./send";

function fakeTerm() {
  const calls: [string, string][] = [];
  return {
    calls,
    paste: (t: string) => calls.push(["paste", t]),
    input: (t: string, user?: boolean) => calls.push(["input", `${t}|${String(user)}`]),
  };
}

const snippet = (text: string, run: boolean) => ({ id: "s", label: "l", text, run });

describe("sendSnippet", () => {
  it("pastes the whole text once and presses nothing without run", () => {
    const t = fakeTerm();
    sendSnippet(t, snippet("echo one\necho two", false));
    expect(t.calls).toEqual([["paste", "echo one\necho two"]]);
  });

  it("presses Enter exactly once after the paste with run", () => {
    const t = fakeTerm();
    sendSnippet(t, snippet("echo one\necho two", true));
    expect(t.calls).toEqual([
      ["paste", "echo one\necho two"],
      ["input", "\r|true"],
    ]);
  });
});

describe("snippetTextOf", () => {
  it("drops the selection's trailing line breaks and nothing else", () => {
    expect(snippetTextOf("df -h\n")).toBe("df -h");
    expect(snippetTextOf("echo one\r\necho two\r\n\n")).toBe("echo one\r\necho two");
    expect(snippetTextOf("  ls -la  ")).toBe("  ls -la  ");
  });
});
