import { describe, it, expect } from "vitest";
import { isCLanguage, cSourceFiles } from "./csrc";
import type { SourceFile } from "./types";

describe("isCLanguage", () => {
  it("recognizes C/C++ language tags (case-insensitive)", () => {
    for (const l of ["c", "cpp", "cc", "cxx", "h", "hpp", "CPP", "C"]) {
      expect(isCLanguage(l)).toBe(true);
    }
  });

  it("treats systemverilog / untagged as not-C", () => {
    expect(isCLanguage("systemverilog")).toBe(false);
    expect(isCLanguage(undefined)).toBe(false);
    expect(isCLanguage(null)).toBe(false);
    expect(isCLanguage("")).toBe(false);
    expect(isCLanguage("verilog")).toBe(false);
  });
});

describe("cSourceFiles", () => {
  const files: SourceFile[] = [
    { id: 0, path: "top.sv", language: "systemverilog" },
    { id: 1, path: "kernel.cpp", language: "cpp" },
    { id: 2, path: "util.sv" }, // untagged ⇒ RTL
    { id: 3, path: "helper.c", language: "c" },
  ];

  it("keeps only the C/C++ files, in table order", () => {
    expect(cSourceFiles(files).map((f) => f.id)).toEqual([1, 3]);
  });

  it("returns nothing for an all-RTL (non-HLS) design", () => {
    const rtl: SourceFile[] = [
      { id: 0, path: "a.sv", language: "systemverilog" },
      { id: 1, path: "b.sv" },
    ];
    expect(cSourceFiles(rtl)).toEqual([]);
  });
});
