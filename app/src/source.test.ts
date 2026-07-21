import { describe, expect, it } from "vitest";

import { highlightLineRange, lineRangeForSpan } from "./source";

// Four lines: "aaaa\nbb\ncccccc\nd" → per-line byte starts (LF = 1 byte).
//   line0 "aaaa"   offsets 0..3, newline 4   → start 0
//   line1 "bb"     offsets 5..6, newline 7   → start 5
//   line2 "cccccc" offsets 8..13, newline 14 → start 8
//   line3 "d"      offset 15                  → start 15
const STARTS = [0, 5, 8, 15];

describe("lineRangeForSpan", () => {
  it("spans the lines a multi-line block covers (exclusive end offset)", () => {
    // start of line1 (5) through end of line2 (last char 13, exclusive end 14)
    expect(lineRangeForSpan(STARTS, 5, 14)).toEqual([1, 2]);
  });

  it("does not over-highlight when the exclusive end sits on a line boundary", () => {
    // line0 only: [0,5) where 5 is the start of line1
    expect(lineRangeForSpan(STARTS, 0, 5)).toEqual([0, 0]);
  });

  it("collapses a degenerate point range to its single line", () => {
    expect(lineRangeForSpan(STARTS, 8, 8)).toEqual([2, 2]);
  });

  it("treats end <= start as a single line", () => {
    expect(lineRangeForSpan(STARTS, 8, 5)).toEqual([2, 2]);
  });

  it("keeps a single-line span on one line", () => {
    expect(lineRangeForSpan(STARTS, 8, 14)).toEqual([2, 2]);
  });

  it("spans the whole file", () => {
    expect(lineRangeForSpan(STARTS, 0, 16)).toEqual([0, 3]);
  });

  it("clamps offsets past the last line start", () => {
    expect(lineRangeForSpan(STARTS, 99, 120)).toEqual([3, 3]);
  });

  it("clamps a negative start offset to the first line", () => {
    expect(lineRangeForSpan(STARTS, -4, 6)).toEqual([0, 1]);
  });

  it("returns [0,0] for empty lineStarts", () => {
    expect(lineRangeForSpan([], 3, 9)).toEqual([0, 0]);
  });
});

describe("highlightLineRange", () => {
  it("maps 1-based lines to an inclusive 0-based span", () => {
    expect(highlightLineRange(1249, 1290)).toEqual([1248, 1289]);
  });

  it("collapses to the single start line when no end is given", () => {
    expect(highlightLineRange(42)).toEqual([41, 41]);
  });

  it("collapses when the end precedes the start", () => {
    expect(highlightLineRange(42, 40)).toEqual([41, 41]);
  });

  it("clamps a first-line construct to 0", () => {
    expect(highlightLineRange(1, 1)).toEqual([0, 0]);
  });
});
