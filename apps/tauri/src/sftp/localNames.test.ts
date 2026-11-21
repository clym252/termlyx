import { describe, expect, it } from "vitest";
import { validateLocalName, windowsLocalName } from "./localNames";

const CHARS = "A name can't contain < > : \" / \\ | ? or *.";
const TRAILING = "A name can't end with a dot or a space.";
const RESERVED = "Windows reserves that name.";

describe("validateLocalName", () => {
  it("rejects each character Windows forbids", () => {
    for (const ch of ["<", ">", ":", '"', "/", "\\", "|", "?", "*"]) {
      expect(validateLocalName(`a${ch}b`, true), ch).toBe(CHARS);
    }
  });

  it("rejects a trailing dot or space", () => {
    expect(validateLocalName("notes.", true)).toBe(TRAILING);
    expect(validateLocalName("notes ", true)).toBe(TRAILING);
    expect(validateLocalName("notes. ", true)).toBe(TRAILING);
    expect(validateLocalName(".gitignore", true)).toBeNull();
    expect(validateLocalName("a b", true)).toBeNull();
  });

  it("rejects reserved device names with or without an extension, in any case", () => {
    const reserved = ["CON", "PRN", "AUX", "NUL", ...[1, 2, 3, 4, 5, 6, 7, 8, 9].flatMap((n) => [`COM${n}`, `LPT${n}`])];
    for (const name of reserved) {
      expect(validateLocalName(name, true), name).toBe(RESERVED);
      expect(validateLocalName(name.toLowerCase(), true), name).toBe(RESERVED);
      expect(validateLocalName(`${name}.txt`, true), name).toBe(RESERVED);
    }
    expect(validateLocalName("con.txt", true)).toBe(RESERVED);
    expect(validateLocalName("Nul.tar.gz", true)).toBe(RESERVED);
    expect(validateLocalName("CON .txt", true)).toBe(RESERVED);
  });

  it("allows names that only look reserved", () => {
    for (const name of ["console", "console.txt", "COM0", "COM10", "LPT0", "com", "nulls", "my.con", "aux1", "Zoë Lee", "日本語.txt"]) {
      expect(validateLocalName(name, true), name).toBeNull();
    }
  });

  it("checks nothing off Windows", () => {
    for (const name of ["a:b", "a?b", "con", "notes.", "notes ", "a*b", 'a"b']) {
      expect(validateLocalName(name, false), name).toBeNull();
    }
  });
});

describe("windowsLocalName", () => {
  it("replaces each forbidden character and control character with an underscore", () => {
    for (const ch of ["<", ">", ":", '"', "/", "\\", "|", "?", "*", "\u0000", "\n", "\t", "\u001f"]) {
      expect(windowsLocalName(`a${ch}b`), JSON.stringify(ch)).toBe("a_b");
    }
    expect(windowsLocalName("a:b.txt")).toBe("a_b.txt");
    expect(windowsLocalName("what?.log")).toBe("what_.log");
    expect(windowsLocalName("x|y")).toBe("x_y");
    expect(windowsLocalName("a\nb\tc")).toBe("a_b_c");
  });

  it("trims trailing dots and spaces, and never leaves an empty name", () => {
    expect(windowsLocalName("notes.")).toBe("notes");
    expect(windowsLocalName("notes ")).toBe("notes");
    expect(windowsLocalName("notes. . ")).toBe("notes");
    expect(windowsLocalName("a.b..")).toBe("a.b");
    expect(windowsLocalName("...")).toBe("_");
    expect(windowsLocalName("   ")).toBe("_");
    expect(windowsLocalName("")).toBe("_");
    // A trailing forbidden character becomes `_`, which is not trimmed.
    expect(windowsLocalName("a:")).toBe("a_");
  });

  it("prefixes reserved device names, with or without an extension, in any case", () => {
    const reserved = ["CON", "PRN", "AUX", "NUL", ...[1, 2, 3, 4, 5, 6, 7, 8, 9].flatMap((n) => [`COM${n}`, `LPT${n}`])];
    for (const name of reserved) {
      expect(windowsLocalName(name), name).toBe(`_${name}`);
      expect(windowsLocalName(name.toLowerCase()), name).toBe(`_${name.toLowerCase()}`);
      expect(windowsLocalName(`${name}.txt`), name).toBe(`_${name}.txt`);
    }
    expect(windowsLocalName("con.txt")).toBe("_con.txt");
    expect(windowsLocalName("Nul.tar.gz")).toBe("_Nul.tar.gz");
    expect(windowsLocalName("CON .txt")).toBe("_CON .txt");
    // Trimmed first: `nul.` is `nul`, which is reserved.
    expect(windowsLocalName("nul.")).toBe("_nul");
  });

  it("leaves names that only look reserved or odd alone", () => {
    for (const name of ["console", "console.txt", "COM0", "COM10", "LPT0", "com", "nulls", "my.con", "aux1"]) {
      expect(windowsLocalName(name), name).toBe(name);
    }
  });

  it("returns legal names unchanged", () => {
    for (const name of ["Zoë Lee", "semi;colon", "日本語.txt", ".gitignore", "a b", "report (1).pdf", "x,y'z", "tilde~#%&{}!@$^`=+[]", "a.b.c", "_"]) {
      expect(windowsLocalName(name), name).toBe(name);
    }
  });

  it("always gives a name validateLocalName accepts", () => {
    const odd = ["a:b", "CON", "con.", "nul:", "COM1.", "a<>b|", "  ", "...", "x\u0001y", "aux .txt", "LPT9 ", 'q"r*', "\\", "?"];
    for (const name of odd) {
      const mapped = windowsLocalName(name);
      expect(validateLocalName(mapped, true), `${JSON.stringify(name)} -> ${JSON.stringify(mapped)}`).toBeNull();
      expect(windowsLocalName(mapped)).toBe(mapped);
    }
  });
});
