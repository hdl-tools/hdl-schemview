import { describe, it, expect } from "vitest";
import { tokenizeLines, type Token } from "./syntax";

// Concatenating a line's token texts must reproduce the line exactly — this is what
// keeps the byte-offset cross-probe correct once a line is many token nodes (#223).
function roundTrips(text: string, lang: string | null = null): boolean {
  const lines = text.split(/\r\n|\r|\n/);
  const toks = tokenizeLines(text, lang);
  return lines.every((ln, i) => (toks[i] ?? []).map((t) => t.text).join("") === ln);
}
const classesOf = (row: Token[]) => row.map((t) => t.cls);

describe("tokenizeLines (SystemVerilog)", () => {
  it("classifies built-in types, leaving identifiers plain", () => {
    const [row] = tokenizeLines("logic clk;");
    expect(row.find((t) => t.text === "logic")?.cls).toBe("type");
    // `clk` is an identifier -> plain (no heuristics; model-driven names are #225)
    expect(row.some((t) => t.text.includes("clk") && t.cls === "plain")).toBe(true);
  });

  it("classifies keywords", () => {
    const [row] = tokenizeLines("always_ff @(posedge clk) begin");
    expect(row.find((t) => t.text === "always_ff")?.cls).toBe("keyword");
    expect(row.find((t) => t.text === "posedge")?.cls).toBe("keyword");
    expect(row.find((t) => t.text === "begin")?.cls).toBe("keyword");
  });

  it("does not match a keyword inside a longer identifier", () => {
    const [row] = tokenizeLines("logical_reset = 1;");
    expect(row.some((t) => t.cls === "type")).toBe(false);
    expect(row.some((t) => t.cls === "keyword")).toBe(false);
  });

  it("classifies sized and plain numeric literals", () => {
    for (const n of ["32'hFF", "4'b10", "8'o17", "'d5", "42"]) {
      const [row] = tokenizeLines(`x = ${n};`);
      expect(row.some((t) => t.text === n && t.cls === "number")).toBe(true);
    }
  });

  it("treats // as a comment but not when inside a string", () => {
    const [c] = tokenizeLines("assign a = b; // note");
    expect(c.some((t) => t.text === "// note" && t.cls === "comment")).toBe(true);
    const [s] = tokenizeLines('s = "a//b";');
    expect(s.some((t) => t.text === '"a//b"' && t.cls === "string")).toBe(true);
    expect(s.some((t) => t.cls === "comment")).toBe(false);
  });

  it("carries a /* */ block comment across lines", () => {
    const rows = tokenizeLines("a /* one\ntwo\nthree */ b");
    expect(rows[0].some((t) => t.text === "/* one" && t.cls === "comment")).toBe(true);
    expect(classesOf(rows[1])).toEqual(["comment"]); // whole middle line
    expect(rows[2].some((t) => t.text === "three */" && t.cls === "comment")).toBe(true);
    expect(rows[2].some((t) => t.text.includes("b") && t.cls === "plain")).toBe(true);
  });

  it("closes a block comment opened and closed on one line", () => {
    const [row] = tokenizeLines("a /* mid */ b");
    expect(row.some((t) => t.text === "/* mid */" && t.cls === "comment")).toBe(true);
    expect(row.some((t) => t.text.includes("b") && t.cls === "plain")).toBe(true);
  });

  it("classifies system tasks and compiler directives by sigil", () => {
    const [t] = tokenizeLines("$display(x);");
    expect(t.some((k) => k.text === "$display" && k.cls === "systask")).toBe(true);
    const [d] = tokenizeLines("`define W 8");
    expect(d.some((k) => k.text === "`define" && k.cls === "directive")).toBe(true);
  });

  it("classifies operator runs", () => {
    const [row] = tokenizeLines("y <= a && b;");
    expect(row.some((t) => t.text === "<=" && t.cls === "operator")).toBe(true);
    expect(row.some((t) => t.text === "&&" && t.cls === "operator")).toBe(true);
  });

  it("round-trips arbitrary source (no chars added or dropped)", () => {
    const src = [
      "`timescale 1ns/1ps",
      "module m(input logic clk); // top",
      "  assign y = 32'hDEAD_BEEF & mask; /* inline */",
      '  always_ff @(posedge clk) $display("hi //not a comment");',
      "endmodule",
    ].join("\n");
    expect(roundTrips(src)).toBe(true);
  });

  it("round-trips across a multi-line block comment", () => {
    expect(roundTrips("a /* one\ntwo\nthree */ b")).toBe(true);
  });

  it("returns a single plain token for an interesting-free line", () => {
    expect(tokenizeLines("  foo_bar baz")[0]).toEqual([
      { text: "  foo_bar baz", cls: "plain" },
    ]);
  });

  it("returns one row per line, including empty lines", () => {
    const rows = tokenizeLines("a\n\nb");
    expect(rows).toHaveLength(3);
    expect(rows[1]).toEqual([]);
  });

  it("claims no keywords for an unregistered language but still lexes strings", () => {
    const [row] = tokenizeLines('char *s = "hi"; // x', "python");
    expect(row.some((t) => t.cls === "keyword" || t.cls === "type")).toBe(false);
    expect(row.some((t) => t.text === '"hi"' && t.cls === "string")).toBe(true);
    expect(row.some((t) => t.text === "// x" && t.cls === "comment")).toBe(true);
  });
});

describe("tokenizeLines (C/C++, #224)", () => {
  it("classifies C keywords and types", () => {
    const [row] = tokenizeLines("static int count = 0;", "c");
    expect(row.find((t) => t.text === "static")?.cls).toBe("keyword");
    expect(row.find((t) => t.text === "int")?.cls).toBe("type");
  });

  it("classifies C++ keywords across the C/C++ language tags", () => {
    const [row] = tokenizeLines("class Foo : public Bar {", "cpp");
    expect(row.find((t) => t.text === "class")?.cls).toBe("keyword");
    expect(row.find((t) => t.text === "public")?.cls).toBe("keyword");
    expect(
      tokenizeLines("namespace n {", "hpp")[0].find((t) => t.text === "namespace")?.cls,
    ).toBe("keyword");
  });

  it("classifies # preprocessor directives", () => {
    const [inc] = tokenizeLines("#include <stdio.h>", "c");
    expect(inc.some((t) => t.text === "#include" && t.cls === "directive")).toBe(true);
    const [def] = tokenizeLines("#define W 8", "cpp");
    expect(def.some((t) => t.text === "#define" && t.cls === "directive")).toBe(true);
  });

  it("does not treat a SystemVerilog backtick directive as a C directive", () => {
    const [row] = tokenizeLines("`define W 8", "c");
    expect(row.some((t) => t.cls === "directive")).toBe(false);
  });

  it("classifies char literals, including escapes", () => {
    expect(
      tokenizeLines("char c = 'a';", "c")[0].some((t) => t.text === "'a'" && t.cls === "string"),
    ).toBe(true);
    expect(
      tokenizeLines("char n = '\\n';", "c")[0].some(
        (t) => t.text === "'\\n'" && t.cls === "string",
      ),
    ).toBe(true);
  });

  it("classifies hex, binary, float, exponent and suffixed numbers", () => {
    for (const n of ["0x1F", "0b1010", "42u", "1.5f", "1e10"]) {
      const [row] = tokenizeLines(`x = ${n};`, "c");
      expect(row.some((t) => t.text === n && t.cls === "number")).toBe(true);
    }
  });

  it("does not classify $ident as a system task in C", () => {
    const [row] = tokenizeLines("int $weird = 1;", "c");
    expect(row.some((t) => t.cls === "systask")).toBe(false);
  });

  it("shares comment and string handling with SystemVerilog", () => {
    const [row] = tokenizeLines('s = "a//b"; /* x */', "cpp");
    expect(row.some((t) => t.text === '"a//b"' && t.cls === "string")).toBe(true);
    expect(row.some((t) => t.text === "/* x */" && t.cls === "comment")).toBe(true);
  });

  it("does not treat a SystemVerilog sized literal as a number in C", () => {
    // `32'hFF` is not C; the leading 32 lexes, the tick does not start a literal.
    const [row] = tokenizeLines("x = 32'hFF;", "c");
    expect(row.some((t) => t.text === "32'hFF")).toBe(false);
    expect(row.some((t) => t.text === "32" && t.cls === "number")).toBe(true);
  });

  it("round-trips C source (no chars added or dropped)", () => {
    const src = [
      "#include <stdint.h>",
      "// add two",
      "int add(int a, int b) { /* sum",
      "   spanning */",
      "  char c = 'x';",
      "  return a + b; // done",
      "}",
    ].join("\n");
    expect(roundTrips(src, "c")).toBe(true);
  });
});
