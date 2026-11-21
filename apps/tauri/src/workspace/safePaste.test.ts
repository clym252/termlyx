import { describe, expect, it, vi } from "vitest";
import { attachSafePaste, safePasteText } from "./safePaste";
import { sendSnippet } from "../snippets/send";

describe("safePasteText", () => {
  it("removes the bracketed-paste end marker and other controls", () => {
    expect(safePasteText("a\x1b[201~curl x|sh\rb\x07\x9b")).toBe("a[201~curl x|sh\rb");
    expect(safePasteText("a\x1b[201~b")).not.toContain("\x1b");
  });
  it("keeps tab, newline and carriage return", () => {
    expect(safePasteText("a\tb\nc\rd")).toBe("a\tb\nc\rd");
  });
  it("keeps unicode and CRLF", () => {
    expect(safePasteText("é漢😀\r\n")).toBe("é漢😀\r\n");
  });
  it("a real paste event is sanitized and never reaches xterm's own listener", () => {
    const el = document.createElement("div");
    const ta = document.createElement("textarea");
    el.append(ta);
    const reached = vi.fn();
    ta.addEventListener("paste", reached);
    const pasted: string[] = [];
    attachSafePaste(el, { paste: (t) => pasted.push(t) });
    const ev = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(ev, "clipboardData", { value: { getData: () => "x\x1b[201~curl e|sh\r" } });
    ta.dispatchEvent(ev);
    expect(pasted).toEqual(["x[201~curl e|sh\r"]);
    expect(reached).not.toHaveBeenCalled();
    expect(ev.defaultPrevented).toBe(true);
  });
  it("sendSnippet pastes the sanitized text", () => {
    const calls: string[] = [];
    sendSnippet({ paste: (t) => calls.push(t), input: () => {} }, { id: "s", label: "l", text: "x\x1b[201~rm -rf ~", run: false });
    expect(calls).toEqual(["x[201~rm -rf ~"]);
  });
});
