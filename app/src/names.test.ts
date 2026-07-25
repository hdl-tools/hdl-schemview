import { describe, it, expect } from "vitest";
import { applyNameRefs, type NameRef } from "./names";
import { tokenizeLines, type Token } from "./syntax";

// Re-tokenize a whole source string, then overlay refs — the exact main.ts pipeline.
function pipeline(src: string, refs: NameRef[]) {
  return applyNameRefs(tokenizeLines(src), refs);
}

const concat = (row: { text: string }[]) => row.map((t) => t.text).join("");

describe("applyNameRefs", () => {
  it("is a no-op with no refs (returns the lexical rows unchanged)", () => {
    const lines = tokenizeLines("assign y = a & b;");
    expect(applyNameRefs(lines, [])).toBe(lines);
  });

  it("preserves the concat invariant on every line", () => {
    const src = "module m;\n  logic clk;\n  assign y = a & b;\nendmodule";
    const refs: NameRef[] = [
      { line: 1, col: 8, len: 1, cls: "module" },
      { line: 2, col: 9, len: 3, cls: "signal" },
      { line: 3, col: 10, len: 1, cls: "signal" },
      { line: 3, col: 14, len: 1, cls: "signal" },
      { line: 3, col: 18, len: 1, cls: "signal" },
    ];
    const rows = pipeline(src, refs);
    src.split("\n").forEach((line, i) => {
      expect(concat(rows[i])).toBe(line);
    });
  });

  it("splits a plain identifier into a name-<cls> token", () => {
    // `  clk` on its own line: leading spaces stay plain, `clk` becomes name-signal.
    const rows = pipeline("  clk", [{ line: 1, col: 3, len: 3, cls: "signal" }]);
    expect(rows[0]).toEqual([
      { text: "  ", cls: "plain" },
      { text: "clk", cls: "name-signal" },
    ]);
  });

  it("never re-tints a non-plain (lexical) token", () => {
    // A ref that lands on the `logic` keyword must not override its `type` class.
    const rows = pipeline("logic", [{ line: 1, col: 1, len: 5, cls: "signal" }]);
    expect(rows[0]).toEqual([{ text: "logic", cls: "type" }]);
  });

  it("colors a qualified reference as a single name token", () => {
    // `bus.valid` is one plain run (the dot is punctuation); the whole reference is
    // one span, so it becomes one name-signal token (concat still exact).
    const rows = pipeline("  bus.valid;", [{ line: 1, col: 3, len: 9, cls: "signal" }]);
    expect(concat(rows[0])).toBe("  bus.valid;");
    expect(rows[0]).toContainEqual({ text: "bus.valid", cls: "name-signal" });
  });

  it("handles two refs inside one plain run", () => {
    // `a & b` — `a` and `b` are plain, `&` is an operator token between them. Both
    // identifiers get colored, the operator is untouched.
    const src = "y = a & b";
    const refs: NameRef[] = [
      { line: 1, col: 5, len: 1, cls: "signal" },
      { line: 1, col: 9, len: 1, cls: "signal" },
    ];
    const row = pipeline(src, refs)[0];
    expect(concat(row)).toBe(src);
    const names = row.filter((t) => t.cls.startsWith("name-"));
    expect(names).toEqual([
      { text: "a", cls: "name-signal" },
      { text: "b", cls: "name-signal" },
    ]);
    // The operator token survived as-is.
    expect(row).toContainEqual({ text: "&", cls: "operator" });
  });

  it("distinguishes classes (param vs signal) via the cls string", () => {
    const rows = pipeline("assign y = W;", [{ line: 1, col: 12, len: 1, cls: "param" }]);
    expect(rows[0]).toContainEqual({ text: "W", cls: "name-param" });
  });
});

// Guard the shape assumption applyNameRefs relies on: a bare identifier stays `plain`.
describe("syntax.ts plain-identifier assumption", () => {
  it("keeps unclassified identifiers plain so refs have something to split", () => {
    const toks: Token[] = tokenizeLines("foo")[0];
    expect(toks).toEqual([{ text: "foo", cls: "plain" }]);
  });
});
