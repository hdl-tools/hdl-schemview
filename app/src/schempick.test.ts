import { describe, it, expect } from "vitest";
import {
  filterSignals,
  isTextEntryTag,
  moveIndex,
  pushTraceStep,
  stepLabel,
  truncateTrace,
} from "./schempick";
import type { SignalEntry, TraceStep } from "./types";

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

// Trace mode's step list (#244 PR3). The backend re-derives the whole trace from
// this list on every change, so what the list contains *is* what gets drawn.
const step = (path: string, dir: TraceStep["dir"], depth?: number): TraceStep => ({
  path,
  dir,
  ...(depth === undefined ? {} : { depth }),
});

describe("pushTraceStep", () => {
  it("appends in order", () => {
    const a = step("top.a", "in");
    const b = step("top.b", "out");
    expect(pushTraceStep(pushTraceStep([], a), b)).toEqual([a, b]);
  });

  it("is idempotent for an identical step", () => {
    // Re-clicking the same affordance should cost nothing, not a re-walk that
    // returns the same graph.
    const a = step("top.a", "in");
    const once = pushTraceStep([], a);
    expect(pushTraceStep(once, step("top.a", "in"))).toBe(once);
  });

  it("keeps the opposite direction of the same signal", () => {
    // This is the case that makes a net's fan-in and fan-out meet at one junction
    // node — deduping it away would silently drop half the connectivity.
    const steps = pushTraceStep(pushTraceStep([], step("top.a", "in")), step("top.a", "out"));
    expect(steps).toHaveLength(2);
  });

  it("treats a different depth as a different step", () => {
    const steps = pushTraceStep(pushTraceStep([], step("top.a", "in", 1)), step("top.a", "in", 3));
    expect(steps).toHaveLength(2);
  });

  it("treats an omitted depth as depth 1", () => {
    const steps = pushTraceStep(pushTraceStep([], step("top.a", "in")), step("top.a", "in", 1));
    expect(steps).toHaveLength(1);
  });

  it("never mutates the input", () => {
    const before: TraceStep[] = [step("top.a", "in")];
    pushTraceStep(before, step("top.b", "out"));
    expect(before).toHaveLength(1);
  });
});

describe("truncateTrace", () => {
  const steps = [step("top.a", "in"), step("top.b", "out"), step("top.c", "in")];

  it("keeps the clicked step and drops what followed it", () => {
    expect(truncateTrace(steps, 0)).toEqual([steps[0]]);
    expect(truncateTrace(steps, 1)).toEqual([steps[0], steps[1]]);
  });

  it("is a no-op on the last step", () => {
    expect(truncateTrace(steps, 2)).toEqual(steps);
  });

  it("leaves the list alone for an out-of-range index", () => {
    // A stale click from a re-rendered bar must not empty the canvas.
    expect(truncateTrace(steps, -1)).toBe(steps);
    expect(truncateTrace(steps, 9)).toBe(steps);
  });
});

describe("stepLabel", () => {
  it("names the signal and the direction it was expanded in", () => {
    expect(stepLabel(step("picorv32_soc.g_lane[0].core.mem_valid", "in"))).toBe("mem_valid ◀");
    expect(stepLabel(step("picorv32_soc.g_lane[0].core.mem_valid", "out"))).toBe("mem_valid ▶");
    expect(stepLabel(step("top.a", "inout"))).toBe("a ◀▶");
  });

  it("survives an indexed or bit-selected path", () => {
    expect(stepLabel(step("top.g_lane[0].bus", "in"))).toBe("bus ◀");
  });

  it("falls back to the whole path when there is no dot", () => {
    expect(stepLabel(step("top", "in"))).toBe("top ◀");
  });
});
