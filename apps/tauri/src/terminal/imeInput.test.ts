import { afterEach, describe, expect, it, vi } from "vitest";
import { attachImeInput, imeDelta } from "./imeInput";

const DEL = "\x7f";

describe("imeDelta", () => {
  it("sends what was added", () => {
    expect(imeDelta("ls ", "ls v")).toBe("v");
    expect(imeDelta("ls v", "ls vv")).toBe("v");
  });

  it("deletes what the keyboard took back, then types the rest", () => {
    expect(imeDelta("ls vvhi", "ls vvh")).toBe(DEL);
    expect(imeDelta("ls teh", "ls the")).toBe(DEL + DEL + "he");
  });

  it("drops ESC and control characters from committed text", () => {
    expect(imeDelta("ls ", "ls \x1b[201~x\x07")).toBe("[201~x");
  });

  it("sends nothing when nothing changed", () => {
    expect(imeDelta("abc", "abc")).toBe("");
  });

  it("types a line break as Enter", () => {
    expect(imeDelta("ls", "ls\n")).toBe("\r");
  });
});

describe("attachImeInput", () => {
  let detach: (() => void) | null = null;
  afterEach(() => {
    detach?.();
    detach = null;
    document.body.innerHTML = "";
  });

  /** A container holding the textarea, a spy standing in for xterm's own listeners, and what was sent. */
  function setup() {
    const container = document.createElement("div");
    const textarea = document.createElement("textarea");
    container.appendChild(textarea);
    document.body.appendChild(container);
    const xterm = vi.fn();
    for (const t of ["compositionstart", "compositionupdate", "compositionend", "input", "keydown"]) {
      textarea.addEventListener(t, xterm);
    }
    const sent: string[] = [];
    detach = attachImeInput(container, textarea, (data) => sent.push(data));
    return { textarea, xterm, sent };
  }

  /** What the keyboard does for one change: beforeinput, the new value, input. */
  function change(textarea: HTMLTextAreaElement, value: string, inputType: string) {
    textarea.dispatchEvent(new InputEvent("beforeinput", { bubbles: true, inputType }));
    textarea.value = value;
    textarea.dispatchEvent(new InputEvent("input", { bubbles: true, inputType }));
  }

  function keydown(textarea: HTMLTextAreaElement, keyCode: number) {
    const ev = new KeyboardEvent("keydown", { bubbles: true, cancelable: true });
    Object.defineProperty(ev, "keyCode", { value: keyCode });
    textarea.dispatchEvent(ev);
  }

  it("sends each letter of a word as the keyboard composes it", () => {
    const { textarea, xterm, sent } = setup();
    textarea.value = "echo ";
    textarea.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
    for (const word of ["h", "hi", "hit"]) {
      keydown(textarea, 229);
      textarea.dispatchEvent(new CompositionEvent("compositionupdate", { bubbles: true, data: word }));
      change(textarea, "echo " + word, "insertCompositionText");
    }
    keydown(textarea, 229);
    textarea.dispatchEvent(new CompositionEvent("compositionupdate", { bubbles: true, data: "hi" }));
    change(textarea, "echo hi", "insertCompositionText");
    textarea.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true, data: "hi" }));
    keydown(textarea, 229);
    change(textarea, "echo hi ", "insertText");

    expect(sent).toEqual(["h", "i", "t", DEL, " "]);
    // xterm never sees the composition or the keyboard's placeholder keys, so it sends nothing twice.
    expect(xterm).not.toHaveBeenCalled();
  });

  it("sends text that comes with no key at all (voice, a picked suggestion)", () => {
    const { textarea, sent } = setup();
    change(textarea, "hello", "insertText");
    change(textarea, "hello world", "insertReplacementText");
    expect(sent).toEqual(["hello", " world"]);
  });

  it("leaves real keys to xterm", () => {
    const { textarea, xterm, sent } = setup();
    keydown(textarea, 8);
    keydown(textarea, 13);
    expect(xterm).toHaveBeenCalledTimes(2);
    expect(sent).toEqual([]);
  });

  it("clears the field on a real key, so the keyboard cannot re-open text the key changed", () => {
    // Samsung Keyboard: "echo", space, then real Backspaces that xterm turns into DELs; the field
    // still held "echo " and the keyboard re-opened "ec" as the word to compose.
    const { textarea, sent } = setup();
    change(textarea, "echo ", "insertText");
    keydown(textarea, 8);
    keydown(textarea, 8);
    expect(textarea.value).toBe("");
    change(textarea, "c", "insertCompositionText");
    expect(sent).toEqual(["echo ", "c"]);
  });

  it("stops after detach", () => {
    const { textarea, xterm, sent } = setup();
    detach?.();
    detach = null;
    change(textarea, "x", "insertText");
    expect(sent).toEqual([]);
    expect(xterm).toHaveBeenCalledTimes(1);
  });
});
