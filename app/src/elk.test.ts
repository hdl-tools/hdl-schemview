import { describe, it, expect } from "vitest";
import { toElk, nodeId, portId, fitZoom } from "./elk";
import type { SchematicGraph } from "./types";

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
