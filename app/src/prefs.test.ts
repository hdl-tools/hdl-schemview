import { describe, it, expect } from "vitest";
import {
  DEFAULT_EXCLUDED,
  parseExcluded,
  formatExcluded,
  coerceExcluded,
} from "./prefs";

describe("parseExcluded", () => {
  it("splits on commas and whitespace, trimming and dropping empties", () => {
    expect(parseExcluded("TOP, tb,  soc_pkg")).toEqual(["TOP", "tb", "soc_pkg"]);
    expect(parseExcluded("TOP\n tb")).toEqual(["TOP", "tb"]);
  });
  it("returns an empty list for blank input", () => {
    expect(parseExcluded("")).toEqual([]);
    expect(parseExcluded("   ,  ")).toEqual([]);
  });
});

describe("formatExcluded", () => {
  it("round-trips with parseExcluded", () => {
    const scopes = ["TOP", "tb", "soc_pkg"];
    expect(parseExcluded(formatExcluded(scopes))).toEqual(scopes);
  });
  it("joins with a comma and space", () => {
    expect(formatExcluded(["a", "b"])).toBe("a, b");
  });
});

describe("coerceExcluded", () => {
  it("falls back to the default when unset", () => {
    expect(coerceExcluded(null)).toEqual(DEFAULT_EXCLUDED);
  });
  it("falls back to the default on malformed JSON", () => {
    expect(coerceExcluded("{not json")).toEqual(DEFAULT_EXCLUDED);
  });
  it("falls back to the default on a non-string-array payload", () => {
    expect(coerceExcluded('"TOP"')).toEqual(DEFAULT_EXCLUDED);
    expect(coerceExcluded("[1,2]")).toEqual(DEFAULT_EXCLUDED);
  });
  it("accepts a stored string array, including empty (exclude nothing)", () => {
    expect(coerceExcluded('["a","b"]')).toEqual(["a", "b"]);
    expect(coerceExcluded("[]")).toEqual([]);
  });
  it("returns a copy, not the shared default array", () => {
    const got = coerceExcluded(null);
    got.push("x");
    expect(DEFAULT_EXCLUDED).toEqual(["TOP", "tb", "soc_pkg"]);
  });
});
