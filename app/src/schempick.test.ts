import { describe, it, expect } from "vitest";
import { filterSignals, isTextEntryTag, moveIndex } from "./schempick";
import type { SignalEntry } from "./types";

const sig = (name: string, path = name): SignalEntry => ({
  path,
  name,
  kind: "Net",
  in_trace: true,
});

describe("filterSignals", () => {
  const sigs = [sig("clk"), sig("reset_n"), sig("mem_ready"), sig("mem_valid")];

  it("returns every signal for a blank or whitespace query", () => {
    expect(filterSignals(sigs, "")).toEqual(sigs);
    expect(filterSignals(sigs, "   ")).toEqual(sigs);
  });

  it("keeps signals whose name contains the query, case-insensitively", () => {
    expect(filterSignals(sigs, "mem").map((s) => s.name)).toEqual([
      "mem_ready",
      "mem_valid",
    ]);
    expect(filterSignals(sigs, "CLK").map((s) => s.name)).toEqual(["clk"]);
  });

  it("trims surrounding whitespace before matching", () => {
    expect(filterSignals(sigs, "  reset ").map((s) => s.name)).toEqual(["reset_n"]);
  });

  it("returns an empty list when nothing matches", () => {
    expect(filterSignals(sigs, "zzz")).toEqual([]);
  });
});

describe("isTextEntryTag", () => {
  it("treats input, textarea and select as text-entry (hotkey suppressed)", () => {
    expect(isTextEntryTag("INPUT", false)).toBe(true);
    expect(isTextEntryTag("TEXTAREA", false)).toBe(true);
    expect(isTextEntryTag("SELECT", false)).toBe(true);
  });

  it("treats a contentEditable element as text-entry regardless of tag", () => {
    expect(isTextEntryTag("DIV", true)).toBe(true);
  });

  it("is false for ordinary elements so the hotkey fires", () => {
    expect(isTextEntryTag("DIV", false)).toBe(false);
    expect(isTextEntryTag("BUTTON", false)).toBe(false);
  });
});

describe("moveIndex", () => {
  it("moves within bounds", () => {
    expect(moveIndex(0, 1, 4)).toBe(1);
    expect(moveIndex(2, -1, 4)).toBe(1);
  });

  it("clamps at both ends rather than wrapping", () => {
    expect(moveIndex(3, 1, 4)).toBe(3); // already at last
    expect(moveIndex(0, -1, 4)).toBe(0); // already at first
  });

  it("returns 0 for an empty list (never a negative index)", () => {
    expect(moveIndex(0, 1, 0)).toBe(0);
    expect(moveIndex(0, -1, 0)).toBe(0);
  });
});
