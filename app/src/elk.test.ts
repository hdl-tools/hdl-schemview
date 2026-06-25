import { describe, it, expect } from "vitest";
import { toElk, nodeId, portId } from "./elk";
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
});
