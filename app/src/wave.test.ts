import { describe, it, expect } from "vitest";
import {
  maxTime,
  isDigital,
  isUnknown,
  buildSegments,
  timeToX,
  xToTime,
  zoomWindow,
  panWindow,
  valueAt,
  valueAtMarker,
  nearestEdge,
  trimBusValue,
  formatValue,
  enumName,
  displayValue,
  sliceBits,
  visibleLabelX,
  tickMarks,
  unitExponent,
  displayScale,
  defaultDisplayUnit,
  reresolveLane,
  laneCounterSeeds,
  flattenLanes,
  visibleLanes,
  withTrailingEmptyGroup,
  normalizeGroups,
  workingGroupIndex,
  type WaveTrace,
  type WaveGroup,
} from "./wave";

const trace = (name: string, values: [number, string][]): WaveTrace => ({
  key: 0,
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

describe("timeToX / xToTime", () => {
  it("maps window endpoints to [0, w]", () => {
    expect(timeToX(100, 100, 200, 400)).toBe(0);
    expect(timeToX(200, 100, 200, 400)).toBe(400);
    expect(timeToX(150, 100, 200, 400)).toBe(200);
  });

  it("round-trips x -> t -> x", () => {
    const x = timeToX(137, 100, 200, 400);
    expect(xToTime(x, 100, 200, 400)).toBeCloseTo(137, 6);
  });

  it("is degenerate-safe for a zero-width window or canvas", () => {
    expect(timeToX(150, 100, 100, 400)).toBe(0);
    expect(xToTime(50, 100, 200, 0)).toBe(100);
  });
});

describe("zoomWindow", () => {
  const max = 1000;
  it("zooms in (factor<1) keeping the pivot's relative position", () => {
    const v = zoomWindow({ t0: 0, t1: 1000 }, 0.5, 500, max);
    expect(v.t1 - v.t0).toBeCloseTo(500, 6);
    expect(v.t0).toBeCloseTo(250, 6);
    expect(v.t1).toBeCloseTo(750, 6);
  });

  it("anchors the pivot time when zooming in off-center", () => {
    const v = zoomWindow({ t0: 0, t1: 1000 }, 0.5, 200, max);
    // pivot at frac 0.2 stays at frac 0.2 of the new 500-wide window
    expect(xToTime(timeToX(200, v.t0, v.t1, 100), v.t0, v.t1, 100)).toBeCloseTo(200, 6);
    expect((200 - v.t0) / (v.t1 - v.t0)).toBeCloseTo(0.2, 6);
  });

  it("clamps the zoomed window inside [0, max]", () => {
    const v = zoomWindow({ t0: 0, t1: 1000 }, 2, 500, max);
    expect(v.t0).toBeGreaterThanOrEqual(0);
    expect(v.t1).toBeLessThanOrEqual(max);
  });
});

describe("panWindow", () => {
  const max = 1000;
  it("shifts the window and preserves its span", () => {
    const v = panWindow({ t0: 100, t1: 300 }, 50, max);
    expect(v).toEqual({ t0: 150, t1: 350 });
  });

  it("clamps to the left edge without shrinking the span", () => {
    const v = panWindow({ t0: 100, t1: 300 }, -500, max);
    expect(v).toEqual({ t0: 0, t1: 200 });
  });

  it("clamps to the right edge without shrinking the span", () => {
    const v = panWindow({ t0: 800, t1: 1000 }, 500, max);
    expect(v).toEqual({ t0: 800, t1: 1000 });
  });
});

describe("valueAt", () => {
  const vals = vc([[0, "0"], [10, "1"], [20, "0"]]);
  it("returns the value held at the time (last change <= t)", () => {
    expect(valueAt(vals, 5)).toBe("0");
    expect(valueAt(vals, 10)).toBe("1");
    expect(valueAt(vals, 15)).toBe("1");
    expect(valueAt(vals, 99)).toBe("0");
  });

  it("returns empty before the first sample", () => {
    expect(valueAt(vals, -1)).toBe("");
    expect(valueAt([], 5)).toBe("");
  });
});

describe("valueAtMarker", () => {
  const vals = vc([[0, "0"], [10, "1"], [20, "0"]]);
  it("shows prev -> next when the marker sits on a transition edge", () => {
    expect(valueAtMarker(vals, 10)).toBe("0 -> 1");
    expect(valueAtMarker(vals, 20)).toBe("1 -> 0");
  });
  it("shows just the value off an edge or at the first sample", () => {
    expect(valueAtMarker(vals, 15)).toBe("1");
    expect(valueAtMarker(vals, 0)).toBe("0");
  });
  it("trims leading zeros on both sides of the transition", () => {
    const bus = vc([[0, "00000000"], [10, "00000018"]]);
    expect(valueAtMarker(bus, 10)).toBe("0 -> 18");
  });
  it("collapses a no-op transition to a single value", () => {
    expect(valueAtMarker(vc([[0, "5"], [10, "5"]]), 10)).toBe("5");
  });
});

describe("formatValue", () => {
  it("converts binary to hex and trims leading zeros", () => {
    expect(formatValue("11000", "hex")).toBe("18");
    expect(formatValue("00011000", "hex")).toBe("18");
    expect(formatValue("1010", "hex")).toBe("a");
    expect(formatValue("0000", "hex")).toBe("0");
  });
  it("converts binary to octal and decimal", () => {
    expect(formatValue("1010", "oct")).toBe("12");
    expect(formatValue("111", "oct")).toBe("7");
    expect(formatValue("11000", "dec")).toBe("24");
  });
  it("passes binary through (trimmed) and single bits", () => {
    expect(formatValue("00011000", "bin")).toBe("11000");
    expect(formatValue("1", "hex")).toBe("1");
    expect(formatValue("", "hex")).toBe("");
  });
  it("yields x when a group/value contains unknown bits", () => {
    expect(formatValue("1x00", "hex")).toBe("x");
    expect(formatValue("1x00", "dec")).toBe("x");
  });
});

describe("enumName", () => {
  const map = new Map([
    [0, "LANE_RESET"],
    [1, "LANE_RUN"],
    [2, "LANE_TRAP"],
  ]);
  it("decodes a binary value to its state name", () => {
    expect(enumName("00", map)).toBe("LANE_RESET");
    expect(enumName("10", map)).toBe("LANE_TRAP");
  });
  it("returns null for unmapped or unknown (x/z) values", () => {
    expect(enumName("11", map)).toBeNull(); // 3 not in map
    expect(enumName("1x", map)).toBeNull();
    expect(enumName("", map)).toBeNull();
  });
});

describe("displayValue", () => {
  const map = new Map([[1, "LANE_RUN"]]);
  it("shows the state name when in name mode and resolvable", () => {
    expect(displayValue("01", "hex", map, true)).toBe("LANE_RUN");
  });
  it("falls back to the radix when not in name mode or unresolvable", () => {
    expect(displayValue("01", "hex", map, false)).toBe("1");
    expect(displayValue("11", "hex", map, true)).toBe("3"); // unmapped -> hex
    expect(displayValue("1x", "hex", map, true)).toBe("x"); // unknown -> radix
  });
});

describe("valueAtMarker name mode", () => {
  const map = new Map([
    [0, "LANE_RESET"],
    [1, "LANE_RUN"],
  ]);
  const vals = vc([[0, "00"], [10, "01"]]);
  it("shows state names across a transition", () => {
    expect(valueAtMarker(vals, 10, "hex", map, true)).toBe("LANE_RESET -> LANE_RUN");
  });
});

describe("visibleLabelX", () => {
  it("centres the label in a fully visible segment", () => {
    expect(visibleLabelX(10, 100, 200, 20)).toBeCloseTo(55, 6);
  });
  it("centres in the visible portion when the segment runs off-screen left", () => {
    // visible span [1, 50] -> centre 25.5
    expect(visibleLabelX(-100, 50, 200, 20)).toBeCloseTo(25.5, 6);
  });
  it("centres in the viewport for a segment spanning the whole view", () => {
    expect(visibleLabelX(-100, 300, 200, 20)).toBeCloseTo(100, 6);
  });
  it("returns null when the visible portion can't fit the label", () => {
    expect(visibleLabelX(-100, 10, 200, 20)).toBeNull();
    expect(visibleLabelX(-200, -10, 200, 20)).toBeNull();
  });
});

describe("sliceBits", () => {
  it("extracts [hi:lo] honoring MSB→LSB bit order", () => {
    expect(sliceBits("00011000", 4, 2)).toBe("110");
    expect(sliceBits("1010", 3, 3)).toBe("1");
    expect(sliceBits("1010", 1, 0)).toBe("10");
  });
  it("clamps out-of-range bounds and rejects hi < lo", () => {
    expect(sliceBits("1010", 99, 0)).toBe("1010");
    expect(sliceBits("1010", 2, -5)).toBe("010");
    expect(sliceBits("1010", 0, 3)).toBe("");
  });
});

describe("valueAtMarker with radix", () => {
  const bus = vc([[0, "00000000"], [10, "00011000"]]);
  it("formats both sides of the transition in the chosen radix", () => {
    expect(valueAtMarker(bus, 10, "hex")).toBe("0 -> 18");
    expect(valueAtMarker(bus, 10, "bin")).toBe("0 -> 11000");
  });
});

describe("nearestEdge", () => {
  const vals = vc([[0, "0"], [10, "1"], [20, "0"]]);
  it("snaps to the closest value-change time", () => {
    expect(nearestEdge(vals, 7)).toBe(10);
    expect(nearestEdge(vals, 4)).toBe(0);
    expect(nearestEdge(vals, 6)).toBe(10);
    expect(nearestEdge(vals, 100)).toBe(20);
  });
  it("returns null for an empty trace", () => {
    expect(nearestEdge([], 5)).toBeNull();
  });
});

describe("trimBusValue", () => {
  it("drops redundant leading zeros", () => {
    expect(trimBusValue("00000018")).toBe("18");
    expect(trimBusValue("00000000")).toBe("0");
    expect(trimBusValue("0")).toBe("0");
    expect(trimBusValue("18")).toBe("18");
  });
  it("keeps non-zero leading characters and empties untouched", () => {
    expect(trimBusValue("0001x0")).toBe("1x0");
    expect(trimBusValue("")).toBe("");
  });
});

describe("tickMarks", () => {
  it("produces evenly spaced ticks within the window at a nice step", () => {
    const ticks = tickMarks(0, 1000, 800);
    expect(ticks.length).toBeGreaterThan(1);
    for (const t of ticks) {
      expect(t.time).toBeGreaterThanOrEqual(0);
      expect(t.time).toBeLessThanOrEqual(1000);
    }
    const step = ticks[1].time - ticks[0].time;
    for (let i = 1; i < ticks.length; i++) {
      expect(ticks[i].time - ticks[i - 1].time).toBeCloseTo(step, 6);
    }
    // nice step is a 1/2/5 x 10^n value
    const mant = step / Math.pow(10, Math.floor(Math.log10(step)));
    expect([1, 2, 5]).toContain(Math.round(mant));
  });

  it("places tick x via timeToX", () => {
    const ticks = tickMarks(0, 1000, 800);
    for (const t of ticks) {
      expect(t.x).toBeCloseTo(timeToX(t.time, 0, 1000, 800), 6);
    }
  });

  it("returns no ticks for a degenerate window", () => {
    expect(tickMarks(100, 100, 800)).toEqual([]);
    expect(tickMarks(0, 1000, 0)).toEqual([]);
  });
});

describe("unitExponent", () => {
  it("maps known units to their power-of-ten seconds exponent", () => {
    expect(unitExponent("ps")).toBe(-12);
    expect(unitExponent("ns")).toBe(-9);
    expect(unitExponent("us")).toBe(-6);
    expect(unitExponent("ms")).toBe(-3);
    expect(unitExponent("s")).toBe(0);
  });
  it("returns null for unknown units", () => {
    expect(unitExponent("")).toBeNull();
    expect(unitExponent("furlong")).toBeNull();
  });
});

describe("displayScale", () => {
  it("is 1 when native and display unit match", () => {
    expect(displayScale({ factor: 1, unit: "ns" }, "ns")).toBeCloseTo(1, 9);
  });
  it("scales by powers of ten between units", () => {
    expect(displayScale({ factor: 1, unit: "ns" }, "us")).toBeCloseTo(1e-3, 12);
    expect(displayScale({ factor: 1, unit: "ns" }, "ps")).toBeCloseTo(1e3, 9);
  });
  it("folds in the native factor", () => {
    expect(displayScale({ factor: 10, unit: "ps" }, "ns")).toBeCloseTo(0.01, 9);
  });
  it("is identity when timescale or unit is unknown", () => {
    expect(displayScale(null, "ns")).toBe(1);
    expect(displayScale({ factor: 1, unit: "" }, "ns")).toBe(1);
  });
});

describe("defaultDisplayUnit", () => {
  it("uses the native unit when it is a selectable one", () => {
    expect(defaultDisplayUnit({ factor: 1, unit: "us" })).toBe("us");
  });
  it("falls back to ns for unknown / out-of-range native units", () => {
    expect(defaultDisplayUnit(null)).toBe("ns");
    expect(defaultDisplayUnit({ factor: 1, unit: "fs" })).toBe("ns");
  });
});

// #179: when a pane swaps its trace, each lane re-resolves by model path. A plain lane
// takes the new trace's ref and values; a sub-bus keeps its synthetic ref and re-slices
// the parent's *new* values (so it stays parent[hi:lo], not the full word) — the fix for
// sub-buses silently vanishing on every trace swap.
describe("reresolveLane", () => {
  it("takes the new ref and full values for a plain lane, keeping identity", () => {
    const tr: WaveTrace = { key: 7, ref: 5, name: "a", path: "top.a", values: vc([[0, "1"]]) };
    const out = reresolveLane(tr, 9, vc([[0, "0"], [10, "1"]]));
    expect(out.ref).toBe(9);
    expect(out.values).toEqual(vc([[0, "0"], [10, "1"]]));
    expect(out.key).toBe(7);
  });

  it("keeps a sub-bus's synthetic ref and re-slices the parent's new values", () => {
    const tr: WaveTrace = {
      key: 8,
      ref: -1,
      name: "a[1:0]",
      path: "top.a",
      slice: { hi: 1, lo: 0 },
      values: vc([]),
    };
    const out = reresolveLane(tr, 9, vc([[0, "1010"]]));
    expect(out.ref).toBe(-1); // synthetic ref preserved, NOT the parent's new ref
    expect(out.values).toEqual(vc([[0, "10"]])); // low two bits of 1010
  });
});

// #179: several lanes can now share a `ref`, and a pop-out reseeded from a snapshot must
// not re-mint a lane key or sub-bus ref that a restored lane already holds. The seeds
// advance past every existing key and below every existing synthetic ref.
describe("laneCounterSeeds", () => {
  it("defaults to 1 / -1 for an empty pane", () => {
    expect(laneCounterSeeds([])).toEqual({ laneKey: 1, derivedRef: -1 });
  });

  it("advances past the largest key and below the smallest ref", () => {
    const seeds = laneCounterSeeds([
      { key: 3, ref: 7 },
      { key: 1, ref: -2 },
    ]);
    expect(seeds).toEqual({ laneKey: 4, derivedRef: -3 });
  });

  it("ignores positive refs when seeding the synthetic-ref counter", () => {
    expect(laneCounterSeeds([{ key: 2, ref: 100 }])).toEqual({ laneKey: 3, derivedRef: -1 });
  });
});

// #182: lanes live inside groups. These pure helpers give the flat views the renderer
// and index-based code still need, and enforce the pane invariant (always ≥1 group, the
// last one empty as a drop target for a new group).
const grp = (collapsed: boolean, ...names: string[]): WaveGroup => ({
  name: "g",
  collapsed,
  waves: names.map((n) => trace(n, [])),
});
const laneNames = (ls: WaveTrace[]) => ls.map((l) => l.name);

describe("flattenLanes", () => {
  it("concatenates every group's lanes in order, collapsed or not", () => {
    const gs = [grp(true, "a"), grp(false, "b", "c")];
    expect(laneNames(flattenLanes(gs))).toEqual(["a", "b", "c"]);
  });
});

describe("visibleLanes", () => {
  it("omits the lanes of collapsed groups", () => {
    const gs = [grp(true, "a"), grp(false, "b", "c")];
    expect(laneNames(visibleLanes(gs))).toEqual(["b", "c"]);
  });
});

describe("withTrailingEmptyGroup", () => {
  it("seeds a single empty group for an empty pane", () => {
    const out = withTrailingEmptyGroup([], () => "G1");
    expect(out).toEqual([{ name: "G1", collapsed: false, waves: [] }]);
  });

  it("appends an empty group when the last is populated", () => {
    const out = withTrailingEmptyGroup([grp(false, "a")], () => "G2");
    expect(out.length).toBe(2);
    expect(out[1].waves).toEqual([]);
  });

  it("is a no-op when the last group is already empty", () => {
    const gs = [grp(false, "a"), grp(false)];
    expect(withTrailingEmptyGroup(gs, () => "X")).toBe(gs);
  });
});

describe("normalizeGroups", () => {
  it("drops interior empties and keeps exactly one trailing empty", () => {
    const gs = [grp(false), grp(false, "a"), grp(false), grp(false, "b")];
    const out = normalizeGroups(gs, () => "NEW");
    expect(laneNames(flattenLanes(out))).toEqual(["a", "b"]);
    expect(out[out.length - 1].waves).toEqual([]);
    expect(out.filter((g) => g.waves.length === 0).length).toBe(1);
  });

  it("yields one empty group for an all-empty (or empty) input", () => {
    expect(normalizeGroups([], () => "NEW")).toEqual([
      { name: "NEW", collapsed: false, waves: [] },
    ]);
  });
});

describe("workingGroupIndex", () => {
  it("is the last populated group when one exists", () => {
    expect(workingGroupIndex([grp(false, "a"), grp(false, "b"), grp(false)])).toBe(1);
  });

  it("falls back to the last group when every group is empty", () => {
    expect(workingGroupIndex([grp(false)])).toBe(0);
  });
});
