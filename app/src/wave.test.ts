import { describe, it, expect } from "vitest";
import { maxTime, isDigital, isUnknown, buildSegments, type WaveTrace } from "./wave";

const trace = (name: string, values: [number, string][]): WaveTrace => ({
  ref: 0,
  name,
  values: values.map(([time, value]) => ({ time, value })),
});
const vc = (pairs: [number, string][]) => pairs.map(([time, value]) => ({ time, value }));

describe("maxTime", () => {
  it("returns 1 for no traces / no samples (degenerate-safe)", () => {
    expect(maxTime([])).toBe(1);
    expect(maxTime([trace("a", [])])).toBe(1);
  });

  it("takes the largest final timestamp across traces", () => {
    const a = trace("a", [[0, "0"], [10, "1"]]);
    const b = trace("b", [[0, "0"], [30, "1"]]);
    expect(maxTime([a, b])).toBe(30);
  });
});

describe("isDigital", () => {
  it("is true for single-character samples (incl. x/z)", () => {
    expect(isDigital(vc([[0, "0"], [1, "x"]]))).toBe(true);
  });

  it("is false when any sample is multi-character (a bus)", () => {
    expect(isDigital(vc([[0, "0"], [1, "1010"]]))).toBe(false);
  });
});

describe("isUnknown", () => {
  it("treats anything but 0/1 as unknown", () => {
    expect(isUnknown("0")).toBe(false);
    expect(isUnknown("1")).toBe(false);
    expect(isUnknown("x")).toBe(true);
    expect(isUnknown("z")).toBe(true);
  });
});

describe("buildSegments", () => {
  it("holds each value until the next change, last to tMax", () => {
    const segs = buildSegments(vc([[0, "0"], [10, "1"]]), 25);
    expect(segs).toEqual([
      { t0: 0, t1: 10, value: "0" },
      { t0: 10, t1: 25, value: "1" },
    ]);
  });

  it("never produces a backwards segment when tMax precedes the last change", () => {
    const segs = buildSegments(vc([[0, "0"], [40, "1"]]), 25);
    expect(segs[segs.length - 1]).toEqual({ t0: 40, t1: 40, value: "1" });
  });
});
