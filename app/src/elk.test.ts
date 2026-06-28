import { describe, it, expect } from "vitest";
import {
  toElk,
  nodeId,
  portId,
  fitZoom,
  FF_W,
  FF_H,
  wireLabelPlacement,
  clampSegmentToRect,
} from "./elk";
import type { SchematicGraph, SchPort } from "./types";

// Build an FF child from a bare port list (FF dispatches to ffChild in toElk).
const ffChildOf = (ports: SchPort[]) =>
  toElk({
    root: "s",
    nodes: [{ id: 1, kind: "FF", label: "FF", path: "s.r", expandable: false, ports }],
    edges: [],
  }).children[0];

const southXs = (c: any): number[] =>
  c.ports
    .filter((p: any) => p.layoutOptions["elk.port.side"] === "SOUTH")
    .map((p: any) => p.x as number)
    .sort((a: number, b: number) => a - b);

const graph: SchematicGraph = {
  root: "top.scope",
  nodes: [
    {
      id: 1,
      kind: "Instance",
      label: "core",
      path: "top.scope.core",
      expandable: true,
      module: "picorv32",
      ports: [
        { id: 10, name: "clk", side: "west" },
        { id: 11, name: "out", side: "east", width: "[31:0]" },
      ],
    },
    { id: 2, kind: "Instance", label: "mem", path: "top.scope.mem", expandable: false, ports: [] },
  ],
  edges: [
    { id: 0, source: 11, target: 2, net: "bus.out" }, // port -> box
  ],
};

describe("toElk", () => {
  it("maps boxes to children with ports on the right sides", () => {
    const elk = toElk(graph);
    expect(elk.children.map((c) => c.id)).toEqual([nodeId(1), nodeId(2)]);
    const core = elk.children[0];
    expect(core.labels[0].text).toBe("core");
    // Pins are placed explicitly so we can inset them below the box top edge.
    expect(core.layoutOptions["elk.portConstraints"]).toBe("FIXED_POS");
    const sides = core.ports.map((p) => p.layoutOptions["elk.port.side"]);
    expect(sides).toEqual(["WEST", "EAST"]);
    // The top pin is shifted down one row pitch (not flush with the top edge).
    expect(core.ports[0].y).toBe(4 + 18);
  });

  it("resolves an edge endpoint to a port when one exists, else the box", () => {
    const elk = toElk(graph);
    expect(elk.edges[0].sources).toEqual([portId(11)]); // 11 is a port
    expect(elk.edges[0].targets).toEqual([nodeId(2)]); // 2 is a box (no port)
  });

  it("carries the net name as an edge label", () => {
    const elk = toElk(graph);
    expect(elk.edges[0].labels?.[0]?.text).toBe("bus.out");
  });

  it("grows a box to fit many long pin labels", () => {
    const big: SchematicGraph = {
      root: "s",
      nodes: [
        {
          id: 1,
          kind: "Instance",
          label: "core",
          path: "s.core",
          expandable: false,
          module: "picorv32",
          ports: Array.from({ length: 8 }, (_, i) => ({
            id: 100 + i,
            name: `mem_la_wstrb_${i}`,
            side: (i % 2 ? "east" : "west") as "east" | "west",
            width: "[31:0]",
          })),
        },
      ],
      edges: [],
    };
    const c = toElk(big).children[0];
    // Far past the 150×58 minimum once real pin labels must fit.
    expect(c.width).toBeGreaterThan(150);
    expect(c.height).toBeGreaterThan(58);
  });

  it("keeps comb nodes compact despite long signal names", () => {
    // Comb draws bare pin stubs (no per-pin labels), so its width must not reserve
    // room for the connected signal names — only the short title.
    const g: SchematicGraph = {
      root: "s",
      nodes: [
        {
          id: 1,
          kind: "Comb",
          label: "comb",
          path: "s.x",
          expandable: false,
          ports: Array.from({ length: 6 }, (_, i) => ({
            id: 200 + i,
            name: `cached_insn_opcode_${i}`,
            side: (i ? "west" : "east") as "east" | "west",
          })),
        },
      ],
      edges: [],
    };
    const c = toElk(g).children[0];
    // Sized from the title, not the long pin names; well under the 150 box floor.
    expect(c.width).toBeLessThan(100);
  });

  it("lays out an assign node as a valid capsule, sized by inputs not names", () => {
    const make = (name: string): SchematicGraph => ({
      root: "s",
      nodes: [
        {
          id: 1,
          kind: "Assign",
          label: "assign",
          path: "s.a",
          expandable: false,
          ports: [
            { id: 10, name, side: "west" },
            { id: 11, name, side: "west" },
            { id: 12, name, side: "west" },
            { id: 13, name, side: "east" },
          ],
        },
      ],
      edges: [],
    });
    const c = toElk(make("x")).children[0];
    // Capsule end caps (radius H/2) are only valid when width >= height.
    expect(c.width).toBeGreaterThanOrEqual(c.height);
    const sides = c.ports.map((p) => p.layoutOptions["elk.port.side"]);
    expect(sides.filter((s) => s === "EAST").length).toBe(1); // single output
    expect(sides.filter((s) => s === "WEST").length).toBe(3); // inputs spread
    // Width is driven by input count, not the (unrendered) signal-name length.
    expect(toElk(make("cached_insn_opcode_wstrb")).children[0].width).toBe(c.width);
  });
});

describe("ffChild", () => {
  const clk: SchPort = { id: 1, name: "clk", side: "west" };
  const q: SchPort = { id: 9, name: "q", side: "east" };
  const data = (id: number): SchPort => ({ id, name: `d${id}`, side: "west" });

  it("keeps the default width for a small FF", () => {
    const c = ffChildOf([clk, data(2), q]); // one data pin
    expect(c.width).toBe(FF_W);
  });

  it("grows only when the data pins exceed the default width", () => {
    const many = Array.from({ length: 10 }, (_, i) => data(20 + i));
    const c = ffChildOf([clk, ...many, q]);
    expect(c.width).toBeGreaterThan(FF_W);
  });

  it("spreads data pins evenly across the bottom edge", () => {
    const c = ffChildOf([clk, data(2), data(3), data(4), data(5), q]); // 4 data
    const xs = southXs(c);
    expect(xs).toHaveLength(4);
    const gaps = xs.slice(1).map((x, i) => x - xs[i]);
    gaps.forEach((g) => expect(g).toBeCloseTo(gaps[0]));
  });

  it("gives each output its own east pin, spread down the wall", () => {
    const out = (id: number): SchPort => ({ id, name: `o${id}`, side: "east" });
    const c = ffChildOf([clk, out(20), out(21), out(22)]); // 3 distinct outputs
    const eastYs = c.ports
      .filter((p) => p.layoutOptions["elk.port.side"] === "EAST")
      .map((p) => p.y as number);
    expect(eastYs).toHaveLength(3);
    expect(new Set(eastYs).size).toBe(3); // all distinct — not collapsed onto one
    expect(c.height).toBeGreaterThan(FF_H); // grew to host the output column
  });

  it("keeps a single output centred at the default height", () => {
    const c = ffChildOf([clk, q]); // one output
    const east = c.ports.filter((p) => p.layoutOptions["elk.port.side"] === "EAST");
    expect(east).toHaveLength(1);
    expect(east[0].y).toBe(FF_H / 2);
  });

  it("centers the reset and keeps data symmetric about it", () => {
    const rst: SchPort = { id: 8, name: "rst_n", side: "west" };
    const c = ffChildOf([clk, rst, data(2), data(3), q]); // 2 data + reset
    const W = c.width;
    const byId = (sid: number) => c.ports.find((p: any) => p.id === portId(sid));
    expect(byId(8)!.x).toBe(W / 2); // reset dead-centre
    expect(byId(2)!.x! + byId(3)!.x!).toBeCloseTo(W); // data mirror about centre
  });
});

describe("wireLabelPlacement", () => {
  it("keeps a horizontal-segment label upright, nudged above the wire", () => {
    const p = wireLabelPlacement({ x: 0, y: 100 }, { x: 80, y: 100 });
    expect(p.rotate).toBe(0);
    expect(p.x).toBe(40);
    expect(p.y).toBe(97); // 100 - 3, above the line
    expect(p.anchor).toBe("middle");
  });

  it("rotates a vertical-segment label to run along the wire", () => {
    const p = wireLabelPlacement({ x: 50, y: 0 }, { x: 50, y: 60 });
    expect(p.rotate).toBe(90);
    expect(p.x).toBe(50);
    expect(p.y).toBe(30); // centred on the segment, no vertical nudge
    expect(p.baseline).toBe("middle");
  });
});

describe("clampSegmentToRect", () => {
  const r = { x0: 0, y0: 0, x1: 100, y1: 100 };

  it("returns a fully-contained segment unchanged", () => {
    const v = clampSegmentToRect({ x: 10, y: 10 }, { x: 90, y: 10 }, r);
    expect(v).toEqual([
      { x: 10, y: 10 },
      { x: 90, y: 10 },
    ]);
  });

  it("clips a segment that runs off the right edge", () => {
    const v = clampSegmentToRect({ x: 50, y: 20 }, { x: 200, y: 20 }, r)!;
    expect(v[0]).toEqual({ x: 50, y: 20 });
    expect(v[1]).toEqual({ x: 100, y: 20 }); // clamped to the rect's right edge
  });

  it("returns null for a segment wholly outside the rect", () => {
    expect(clampSegmentToRect({ x: 200, y: 200 }, { x: 300, y: 250 }, r)).toBeNull();
  });
});

describe("fitZoom", () => {
  it("shrinks a large graph to fit the viewport", () => {
    // 2000x1500 graph into an 800x600 pane: limited by the tighter of the two
    // ratios (both 0.4 here), well under 1.
    expect(fitZoom(2000, 1500, 800, 600)).toBeCloseTo(0.4);
  });

  it("picks the tighter axis ratio", () => {
    // Wide-but-short graph: width is the binding constraint.
    expect(fitZoom(1600, 300, 800, 600)).toBeCloseTo(0.5);
  });

  it("never magnifies a small graph past 100%", () => {
    expect(fitZoom(200, 150, 800, 600)).toBe(1);
  });

  it("falls back to 100% when the pane is unmeasurable", () => {
    // jsdom/hidden panes report 0 for clientWidth/Height — don't divide by zero.
    expect(fitZoom(2000, 1500, 0, 0)).toBe(1);
  });
});
