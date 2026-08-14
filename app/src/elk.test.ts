import { describe, it, expect } from "vitest";
import {
  toElk,
  layout,
  nodeId,
  portId,
  fitZoom,
  ffRole,
  FF_W,
  FF_H,
  FF_TOP,
  FF_CLK_ZONE,
  MEM_W,
  MEM_H,
  MUX_W,
  MUX_H,
  GATE_W,
  GATE_H,
  isGateKind,
  trunkGroups,
  gatherBar,
  wireLabelPlacement,
  clampSegmentToRect,
  segmentsAabb,
  longestSegment,
  rectsIntersect,
  labelGeometry,
  chooseLabelSegment,
  placementsEqual,
  AFFORD_SOUTH_DROP,
  CONTAINER_LABEL_H,
  CONTAINER_PAD,
} from "./elk";
import type { LabelPlacement, Pt, VRect } from "./elk";
import type { SchematicGraph, SchPort } from "./types";

// Build an FF child from a bare port list (FF dispatches to ffChild in toElk).
const ffChildOf = (ports: SchPort[]) =>
  toElk({
    root: "s",
    nodes: [{ id: 1, kind: "FF", label: "FF", path: "s.r", expandable: false, ports }],
    edges: [],
  }).children[0];

const westYs = (c: any): number[] =>
  c.ports
    .filter((p: any) => p.layoutOptions["elk.port.side"] === "WEST")
    .map((p: any) => p.y as number)
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

// #293 — an instance the trace descended through becomes a container, and the
// boxes behind its wall are drawn inside it rather than as its peers.
const contained: SchematicGraph = {
  root: "top",
  nodes: [
    {
      id: 1,
      kind: "Instance",
      label: "core",
      path: "top.core",
      expandable: true,
      ports: [{ id: 10, name: "mem_valid", side: "east" }],
    },
    // Behind core's wall.
    {
      id: 2,
      kind: "FF",
      label: "$ff2",
      path: "top.core.$ff2",
      expandable: false,
      parent: 1,
      ports: [{ id: 20, name: "q", side: "east" }],
    },
    // Outside it, at the top level.
    { id: 3, kind: "Instance", label: "mem", path: "top.mem", expandable: false, ports: [] },
  ],
  edges: [{ id: 0, source: 20, target: 10, net: "mem_valid" }],
};

describe("south-wall affordance room (#286)", () => {
  const muxGraph = (on: boolean) =>
    toElk(
      {
        root: "s",
        nodes: [
          {
            id: 1,
            kind: "Mux",
            label: "mux",
            path: "s.m",
            expandable: false,
            ports: [
              { id: 10, name: "a", side: "west", path: "s.a" },
              { id: 11, name: "y", side: "east", path: "s.y" },
              { id: 12, name: "s", side: "west", role: "sel", path: "s.sel" },
            ],
          },
        ],
        edges: [],
      },
      { affordances: on },
    ).children[0];

  it("reserves a band below a box with a south pin", () => {
    // The select's control drops below the box, where the west/east gutters
    // reserve nothing — without this it lands on whatever ELK puts underneath.
    const m = muxGraph(true).layoutOptions["elk.margins"];
    expect(m).toContain(`bottom=${AFFORD_SOUTH_DROP.toFixed(1)}`);
  });

  it("reserves nothing below when affordances are off", () => {
    const m = muxGraph(false).layoutOptions["elk.margins"];
    expect(m === undefined || m.includes("bottom=0")).toBe(true);
  });
});

describe("containment (#293)", () => {
  it("draws a contained box inside its container, not beside it", () => {
    const e = toElk(contained);
    expect(e.children.map((c) => c.id)).toEqual([nodeId(1), nodeId(3)]);
    const core = e.children.find((c) => c.id === nodeId(1))!;
    expect(core.children?.map((c) => c.id)).toEqual([nodeId(2)]);
    // The peer must not also appear at the top level, or it is drawn twice.
    expect(e.children.some((c) => c.id === nodeId(2))).toBe(false);
  });

  it("turns a container's pins side-only so they stay on the wall when it grows", () => {
    // FIXED_POS coordinates were measured against the opaque box's size; ELK
    // resizes a compound from its children, so keeping them would strand every
    // pin somewhere in the interior.
    const core = toElk(contained).children.find((c) => c.id === nodeId(1))!;
    expect(core.layoutOptions["elk.portConstraints"]).toBe("FIXED_SIDE");
    for (const p of core.ports) {
      expect(p.x).toBeUndefined();
      expect(p.y).toBeUndefined();
    }
    expect(core.layoutOptions["elk.padding"]).toContain(
      `top=${CONTAINER_LABEL_H + CONTAINER_PAD}`,
    );
  });

  it("asks ELK to route across container walls", () => {
    const e = toElk(contained);
    // Without INCLUDE_CHILDREN the default is SEPARATE_CHILDREN, which lays each
    // compound out alone and will not route the ff -> port wire at all.
    expect(e.layoutOptions["elk.hierarchyHandling"]).toBe("INCLUDE_CHILDREN");
    // And deliberately *not* edgeCoords: elkjs 0.9.3 ignores it under either
    // option id, silently, so the wires drew offset by their container's origin
    // with nothing to say why. The renderer rebases from each edge's reported
    // `container` instead. Asserted so nobody re-adds it and trusts it.
    expect(e.layoutOptions["elk.json.edgeCoords"]).toBeUndefined();
    expect(e.layoutOptions["org.eclipse.elk.json.edgeCoords"]).toBeUndefined();
  });

  it("leaves a graph with no containment exactly as it was", () => {
    // The hierarchy view shows one scope and so never nests. It must reach ELK
    // with the same option set and the same flat shape as before #293, or its
    // layout shifts for a feature it does not use.
    const e = toElk(graph);
    expect(e.layoutOptions["elk.hierarchyHandling"]).toBeUndefined();
    expect(e.layoutOptions["elk.json.edgeCoords"]).toBeUndefined();
    expect(e.children.every((c) => c.children === undefined)).toBe(true);
    expect(e.children.map((c) => c.id)).toEqual([nodeId(1), nodeId(2)]);
  });

  it("keeps a box whose container is not on canvas rather than dropping it", () => {
    // The backend guarantees a parent is always drawn; if that ever breaks,
    // drawing the box at the top level is a far better failure than losing it.
    const orphaned: SchematicGraph = {
      ...contained,
      nodes: [contained.nodes[1], contained.nodes[2]],
    };
    const e = toElk(orphaned);
    expect(e.children.map((c) => c.id)).toEqual([nodeId(2), nodeId(3)]);
  });

  it("lands a cross-wall wire on the pins it connects, in root coordinates", async () => {
    // The renderer draws wires into the root group but draws boxes into nested
    // groups, so a routed point only meets its pin if ELK reports edge geometry
    // in root space. If it reports container-relative coordinates instead, every
    // wire inside a container floats off by that container's origin — visible,
    // wrong, and silent.
    const laid: any = await layout(contained);
    const abs = (id: string, kids: any[] = laid.children, dx = 0, dy = 0): any => {
      for (const c of kids) {
        const x = dx + (c.x ?? 0);
        const y = dy + (c.y ?? 0);
        if (c.id === id) return { c, x, y };
        const hit = abs(id, c.children ?? [], x, y);
        if (hit) return hit;
      }
      return undefined;
    };
    const portAbs = (nid: string, pid: string) => {
      const box = abs(nid)!;
      const p = box.c.ports.find((q: any) => q.id === pid)!;
      return { x: box.x + (p.x ?? 0), y: box.y + (p.y ?? 0) };
    };
    const ff = portAbs(nodeId(2), portId(20)); // inside the container
    const wall = portAbs(nodeId(1), portId(10)); // on the container's wall
    const e = (laid.edges ?? []).find((x: any) => x.id === "e0");
    expect(e?.sections?.length, "the cross-wall edge must survive layout").toBeTruthy();
    // ELK reports the points relative to the node it names in `container`, so
    // rebase exactly as the renderer does. This is the contract the renderer
    // depends on: if ELK ever reported root coordinates instead, `container`
    // would be absent and the offset would fall back to zero.
    const org = e.container ? abs(String(e.container)) : undefined;
    const off = { x: org?.x ?? 0, y: org?.y ?? 0 };
    const at = (p: any) => ({ x: p.x + off.x, y: p.y + off.y });
    const sec = e.sections[0];
    const near = (a: any, b: any) => Math.hypot(a.x - b.x, a.y - b.y);
    // Generous tolerance: we are catching a whole-container offset, not pixels.
    expect(near(at(sec.startPoint), ff)).toBeLessThan(12);
    expect(near(at(sec.endPoint), wall)).toBeLessThan(12);
  });

  it("routes every wire of a container wired on both walls", async () => {
    // The shape a real trace produces (#286): a container with pins on both
    // walls, several children, a wire in from outside, and wires between the
    // children and the container's own pins in both directions. The narrow
    // fixture above has one child and one wire, and passed while the real graph
    // lost a wire.
    const g: SchematicGraph = {
      root: "top",
      nodes: [
        {
          id: 1,
          kind: "Instance",
          label: "core",
          path: "top.core",
          expandable: true,
          ports: [
            { id: 10, name: "mem_valid", side: "east", path: "top.core.mem_valid" },
            { id: 11, name: "mem_ready", side: "west", path: "top.core.mem_ready" },
          ],
        },
        {
          id: 2,
          kind: "FF",
          label: "$ff2",
          path: "top.core.$ff2",
          expandable: false,
          parent: 1,
          ports: [{ id: 20, name: "q", side: "east", path: "top.core.q" }],
        },
        {
          id: 3,
          kind: "And",
          label: "and",
          path: "top.core.$and3",
          expandable: false,
          parent: 1,
          ports: [
            { id: 30, name: "a", side: "west", path: "top.core.mem_ready" },
            { id: 31, name: "y", side: "east", path: "top.core.y" },
          ],
        },
        {
          id: 4,
          kind: "Interface",
          label: "bus",
          path: "top.bus",
          expandable: true,
          ports: [{ id: 40, name: "b", side: "east", path: "top.bus.b" }],
        },
      ],
      edges: [
        { id: 0, source: 20, target: 10, net: "mem_valid" }, // child -> container east pin
        { id: 1, source: 40, target: 11, net: "mem_ready" }, // outside -> container west pin
        { id: 2, source: 11, target: 30, net: "mem_ready" }, // container west pin -> child
      ],
    };
    const laid: any = await layout(g);
    const collect = (kids: any[]): any[] =>
      kids.flatMap((c) => [c, ...collect(c.children ?? [])]);
    const all = collect(laid.children ?? []);
    const everywhere = [...(laid.edges ?? []), ...all.flatMap((c: any) => c.edges ?? [])];
    for (const id of ["e0", "e1", "e2"]) {
      const e = everywhere.find((x: any) => x.id === id);
      expect(e, `${id} must survive layout`).toBeTruthy();
      expect(e.sections?.length, `${id} must be routed`).toBeGreaterThan(0);
      // And it must be reachable from the root edge list, which is the only
      // place the renderer looks.
      expect(
        (laid.edges ?? []).some((x: any) => x.id === id),
        `${id} was relocated out of the root edge list, where the renderer cannot see it`,
      ).toBe(true);
    }
  });

  it("routes a wire that crosses the container wall", async () => {
    // The point of the whole option set: a ff inside core wired to core's own
    // boundary pin must still produce a routed edge.
    const laid: any = await layout(contained);
    const e = (laid.edges ?? []).find((x: any) => x.id === "e0");
    expect(e, "the cross-wall edge must survive layout").toBeTruthy();
    expect(e.sections?.length).toBeGreaterThan(0);
  });
});

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

  it("clusters a modport-qualified bundle at the boundary frame", () => {
    // #106/#125: inside the consumer, its modport-qualified interface port is
    // the module's window to the outside — a frame pin sharing the _SEPARATE
    // frame column with the scope's own boundary pins, so the square lines up
    // with them. A mostly-in view (pins mirrored east, feeding the design)
    // sits first; a mostly-out view last. A bare interface instance (no
    // modport) stays a free box.
    const bundle = (modport: string | undefined, sides: ("east" | "west")[]): SchematicGraph => ({
      root: "s.memory",
      nodes: [
        {
          id: 1,
          kind: "Interface",
          label: "bus",
          path: "s.memory.bus",
          expandable: false,
          module: "mem_if",
          modport,
          ports: sides.map((side, i) => ({ id: 10 + i, name: `m${i}`, side })),
        },
      ],
      edges: [],
    });
    const mostlyIn = toElk(bundle("mem", ["east", "east", "east", "west"])).children[0];
    expect(mostlyIn.layoutOptions["elk.layered.layering.layerConstraint"]).toBe("FIRST_SEPARATE");
    // #125: the modport bundle is a frame *pin*, not a box — a pin-height node
    // whose wired members all anchor at the square glyph on the design-facing
    // wall; with no edges in this fixture it carries no ports at all.
    expect(mostlyIn.layoutOptions["elk.portConstraints"]).toBe("FIXED_POS");
    expect(mostlyIn.height).toBe(26);
    expect(mostlyIn.ports).toHaveLength(0);
    const mostlyOut = toElk(bundle("core", ["west", "west", "east"])).children[0];
    expect(mostlyOut.layoutOptions["elk.layered.layering.layerConstraint"]).toBe("LAST_SEPARATE");
    const bare = toElk(bundle(undefined, ["east", "west"])).children[0];
    expect(bare.layoutOptions["elk.layered.layering.layerConstraint"]).toBeUndefined();
    // A bare interface *instance* stays the full hexagon box.
    expect(bare.layoutOptions["elk.portConstraints"]).toBe("FIXED_POS");
    expect(bare.height).toBeGreaterThan(58);
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

  it("lays out an assign node as a small anonymous square (#135)", () => {
    const make = (name: string, inputs = 3): SchematicGraph => ({
      root: "s",
      nodes: [
        {
          id: 1,
          kind: "Assign",
          label: "assign",
          path: "s.a",
          expandable: false,
          ports: [
            ...Array.from({ length: inputs }, (_, i) => ({
              id: 10 + i,
              name,
              side: "west" as const,
            })),
            { id: 20, name, side: "east" as const },
          ],
        },
      ],
      edges: [],
    });
    const c = toElk(make("x")).children[0];
    // Fixed 16px width; height grows a couple px per input (max(16, 4 + rows*6)).
    expect(c.width).toBe(16);
    expect(c.height).toBe(22);
    expect(toElk(make("x", 1)).children[0].height).toBe(16);
    // Anonymous: the "assign" text label is gone — wire net labels carry the meaning.
    expect(c.labels).toHaveLength(0);
    const westPorts = c.ports.filter((p) => p.layoutOptions["elk.port.side"] === "WEST");
    const eastPorts = c.ports.filter((p) => p.layoutOptions["elk.port.side"] === "EAST");
    expect(eastPorts.length).toBe(1); // single output
    expect(westPorts.length).toBe(3); // inputs spread
    // Ports sit flush on the walls — the capsule-curve inset is gone.
    for (const p of westPorts) expect(p.x).toBe(0);
    expect(eastPorts[0].x).toBe(16);
    // Width is unaffected by the (unrendered) signal-name length.
    expect(toElk(make("cached_insn_opcode_wstrb")).children[0].width).toBe(16);
  });
});

describe("memory child (#112)", () => {
  const memOf = (ports: SchPort[], extra: Partial<SchematicGraph["nodes"][0]> = {}) =>
    toElk({
      root: "s",
      nodes: [
        {
          id: 1,
          kind: "Memory",
          label: "ram",
          path: "s.ram",
          expandable: false,
          memDepth: 512,
          ports,
          ...extra,
        },
      ],
      edges: [],
    }).children[0];

  const addr: SchPort = { id: 10, name: "word_idx", side: "west", role: "addr" };
  const din: SchPort = { id: 11, name: "wdata", side: "west", role: "din", width: "[31:0]" };
  const dout: SchPort = { id: 12, name: "rdata", side: "east", role: "dout", width: "[31:0]" };

  it("puts addr/din on the west wall and dout on the east", () => {
    const c = memOf([addr, din, dout]);
    expect(c.id).toBe(nodeId(1));
    expect(c.labels[0].text).toBe("ram");
    expect(c.layoutOptions["elk.portConstraints"]).toBe("FIXED_POS");
    const west = c.ports.filter((p) => p.layoutOptions["elk.port.side"] === "WEST");
    const east = c.ports.filter((p) => p.layoutOptions["elk.port.side"] === "EAST");
    expect(west.map((p) => p.id)).toEqual([portId(10), portId(11)]);
    expect(east.map((p) => p.id)).toEqual([portId(12)]);
    for (const p of west) expect(p.x).toBe(0);
    expect(east[0].x).toBe(c.width);
  });

  it("never shrinks below the memory floor size", () => {
    // A single short-labelled pin each side stays at the floor; real role/width
    // labels only grow it.
    const tiny = memOf([{ id: 10, name: "a", side: "west", role: "addr" }], {});
    expect(tiny.width).toBeGreaterThanOrEqual(MEM_W);
    expect(tiny.height).toBeGreaterThanOrEqual(MEM_H);
    const c = memOf([addr, din, dout]);
    expect(c.width).toBeGreaterThanOrEqual(MEM_W);
    // Two west rows grow the box past the single-row floor.
    expect(c.height).toBeGreaterThan(MEM_H);
  });

  it("grows height with more pin rows", () => {
    const many = Array.from({ length: 6 }, (_, i) => ({
      id: 20 + i,
      name: `a${i}`,
      side: "west" as const,
      role: "addr" as const,
    }));
    expect(memOf([...many, dout]).height).toBeGreaterThan(MEM_H);
  });
});

describe("interface bundle", () => {
  it("pads the box height so wall pins clear the hexagon caps", () => {
    const asKind = (kind: string) =>
      toElk({
        root: "s",
        nodes: [
          {
            id: 1,
            kind,
            label: "bus",
            path: "s.bus",
            expandable: false,
            module: "mem_if",
            ports: [
              { id: 10, name: "valid", side: "west" },
              { id: 11, name: "ready", side: "east" },
            ],
          },
        ],
        edges: [],
      }).children[0];
    const iface = asKind("Interface");
    const box = asKind("Instance");
    // Same pin rows, but the bundle reserves room for its pointed caps.
    expect(iface.height).toBeGreaterThan(box.height);
    // Pins keep their wall positions (below the top cap).
    expect(iface.ports[0].y).toBeGreaterThan(12);
  });

  it("gives a modport-qualified port the compact pin shape, no cap pad (#125)", () => {
    // The hexagon (and its IFACE_CAP pad) is the interface-*instance* glyph; a
    // modport-qualified port is drawn as a square frame pin instead.
    const c = toElk({
      root: "s.memory",
      nodes: [
        {
          id: 1,
          kind: "Interface",
          label: "bus",
          path: "s.memory.bus",
          expandable: false,
          module: "mem_if",
          modport: "mem",
          ports: [
            { id: 10, name: "valid", side: "west" },
            { id: 11, name: "ready", side: "east" },
          ],
        },
      ],
      edges: [],
    }).children[0];
    expect(c.height).toBe(26);
  });
});

describe("modport bundle pin (#125)", () => {
  // The real drilled soc_mem topology (fixtures golden): a modport bundle
  // (6 east / 2 west members), the inferred FF, and the scope's own resetn
  // boundary pin. instr/addr have no edges — unconnected in this scope.
  const port = (id: number, name: string, side: "east" | "west", path: string): SchPort => ({
    id,
    name,
    side,
    path,
  });
  const drilled: SchematicGraph = {
    root: "top.g_lane[0].memory",
    nodes: [
      {
        id: 375,
        kind: "Interface",
        label: "bus",
        path: "top.g_lane[0].memory.bus",
        expandable: false,
        module: "mem_if",
        modport: "mem",
        ports: [
          port(376, "clk", "east", "top.g_lane[0].bus.clk"),
          port(377, "valid", "east", "top.g_lane[0].bus.valid"),
          port(378, "instr", "east", "top.g_lane[0].bus.instr"),
          port(379, "addr", "east", "top.g_lane[0].bus.addr"),
          port(380, "wdata", "east", "top.g_lane[0].bus.wdata"),
          port(381, "wstrb", "east", "top.g_lane[0].bus.wstrb"),
          port(382, "ready", "west", "top.g_lane[0].bus.ready"),
          port(383, "rdata", "west", "top.g_lane[0].bus.rdata"),
        ],
      },
      {
        id: 389,
        kind: "FF",
        label: "$ff389",
        path: "top.g_lane[0].memory.$ff389",
        expandable: false,
        ports: [
          { id: 1000, name: "clk", side: "west", role: "clk" },
          { id: 1001, name: "valid", side: "west" },
          { id: 1002, name: "ready", side: "east" },
          { id: 1003, name: "wdata", side: "west" },
          { id: 1004, name: "wstrb", side: "west" },
          { id: 1005, name: "rdata", side: "east" },
          { id: 1006, name: "resetn", side: "west" },
        ],
      },
      {
        id: 384,
        kind: "Port",
        label: "resetn",
        path: "top.g_lane[0].memory.resetn",
        expandable: false,
        ports: [port(384, "resetn", "east", "top.g_lane[0].memory.resetn")],
      },
    ],
    edges: [
      { id: 1222, source: 376, target: 1000, net: "clk", net_path: "top.g_lane[0].bus.clk" },
      { id: 1223, source: 377, target: 1001, net: "valid", net_path: "top.g_lane[0].bus.valid" },
      { id: 1224, source: 1002, target: 382, net: "ready", net_path: "top.g_lane[0].bus.ready" },
      { id: 1225, source: 380, target: 1003, net: "wdata", net_path: "top.g_lane[0].bus.wdata" },
      { id: 1226, source: 381, target: 1004, net: "wstrb", net_path: "top.g_lane[0].bus.wstrb" },
      { id: 1227, source: 1005, target: 383, net: "rdata", net_path: "top.g_lane[0].bus.rdata" },
      {
        id: 1228,
        source: 384,
        target: 1006,
        net: "resetn",
        net_path: "top.g_lane[0].memory.resetn",
      },
    ],
  };

  it("emits the bundle as a compact frame pin with only wired members", () => {
    const bundle = toElk(drilled).children[0];
    expect(bundle.id).toBe(nodeId(375));
    expect(bundle.layoutOptions["elk.layered.layering.layerConstraint"]).toBe("FIRST_SEPARATE");
    expect(bundle.layoutOptions["elk.portConstraints"]).toBe("FIXED_POS");
    // 6 of 8 members carry edges (instr/addr are unconnected — omitted); all
    // anchor at the square glyph on the design-facing wall, so every member
    // wire visually meets the pin.
    expect(bundle.ports).toHaveLength(6);
    for (const p of bundle.ports) {
      expect(p.layoutOptions["elk.port.side"]).toBe("EAST");
      expect(p.x).toBe(bundle.width);
      expect(p.y).toBe(bundle.height / 2);
    }
    expect(bundle.height).toBe(26);
    // Wide enough for the sublabel "(mem_if.mem)" beside the square.
    expect(bundle.width).toBeGreaterThanOrEqual(98);
    expect(bundle.labels[0].text).toBe("bus");
  });

  it("keeps every member-tap edge — no trunk grouping without bundle flags", () => {
    expect(trunkGroups(drilled)).toHaveLength(0);
    const elk = toElk(drilled);
    expect(elk.edges.map((e: any) => e.id).sort()).toEqual([
      "e1222",
      "e1223",
      "e1224",
      "e1225",
      "e1226",
      "e1227",
      "e1228",
    ]);
    for (const e of elk.edges as any[]) expect(e.labels?.[0]?.text).toBeTruthy();
  });

  it("lays out the bundle inside the canvas, left of the FF", async () => {
    const laid = await layout(drilled);
    const child = (id: number) => laid.children.find((c: any) => c.id === nodeId(id));
    const bundle = child(375)!;
    const ff = child(389)!;
    const resetn = child(384)!;
    for (const c of [bundle, ff, resetn]) {
      expect(typeof c.x).toBe("number");
      expect(typeof c.y).toBe("number");
    }
    // The bundle shares the _SEPARATE frame column with resetn (#125): both
    // frame pins occupy the same layer — horizontally overlapping, aligned at
    // the design-facing wall — left of the FF that consumes them.
    expect(bundle.x).toBeLessThan(resetn.x + resetn.width);
    expect(resetn.x).toBeLessThan(bundle.x + bundle.width);
    expect(bundle.x + bundle.width).toBeCloseTo(resetn.x + resetn.width, 5);
    expect(bundle.x + bundle.width).toBeLessThanOrEqual(ff.x);
    // Every routed point stays inside the reported canvas (regression for the
    // out-of-bounds theory from the original #125 report).
    for (const e of laid.edges ?? []) {
      for (const s of e.sections ?? []) {
        for (const p of [s.startPoint, ...(s.bendPoints ?? []), s.endPoint]) {
          expect(p.x).toBeGreaterThanOrEqual(0);
          expect(p.x).toBeLessThanOrEqual(laid.width);
          expect(p.y).toBeGreaterThanOrEqual(0);
          expect(p.y).toBeLessThanOrEqual(laid.height);
        }
      }
    }
  });
});

describe("ffRole", () => {
  it("prefers the model role fact over name conventions (#59)", () => {
    // Neither name matches the clk/rst regexes — only the model fact can
    // classify these correctly.
    expect(ffRole({ id: 1, name: "gated_ck", side: "west", role: "clk" })).toBe("clk");
    expect(ffRole({ id: 2, name: "arst", side: "west", role: "reset" })).toBe("reset");
  });

  it("falls back to conventional names when no fact exists (sync reset)", () => {
    expect(ffRole({ id: 3, name: "resetn", side: "west" })).toBe("reset");
    expect(ffRole({ id: 4, name: "clk", side: "west" })).toBe("clk");
    expect(ffRole({ id: 5, name: "d", side: "west" })).toBe("data");
  });

  it("keeps east pins as Q regardless of role", () => {
    expect(ffRole({ id: 6, name: "q", side: "east" })).toBe("q");
  });

  it("honors the model enable fact (#59)", () => {
    // "gate" matches no name convention — only the fact can classify it.
    expect(ffRole({ id: 7, name: "gate", side: "west", role: "enable" })).toBe("enable");
  });

  it("falls back to conventional enable names when no fact exists", () => {
    expect(ffRole({ id: 8, name: "en", side: "west" })).toBe("enable");
    expect(ffRole({ id: 9, name: "ce", side: "west" })).toBe("enable");
    expect(ffRole({ id: 10, name: "wr_en", side: "west" })).toBe("enable");
    // Word-bounded: names merely containing "en"/"ce" stay data.
    expect(ffRole({ id: 11, name: "wen", side: "west" })).toBe("data");
    expect(ffRole({ id: 12, name: "end", side: "west" })).toBe("data");
  });
});

describe("ffChild", () => {
  const clk: SchPort = { id: 1, name: "clk", side: "west" };
  const q: SchPort = { id: 9, name: "q", side: "east" };
  const data = (id: number): SchPort => ({ id, name: `d${id}`, side: "west" });
  const byId = (c: any, sid: number) => c.ports.find((p: any) => p.id === portId(sid));

  it("keeps the default size for a small FF", () => {
    const c = ffChildOf([clk, data(2), q]); // one short data pin
    expect(c.width).toBe(FF_W);
    expect(c.height).toBe(FF_H);
  });

  it("stacks data pins as labelled rows down the west wall (#115)", () => {
    const c = ffChildOf([clk, data(2), data(3), data(4), q]); // 3 data
    const ys = westYs(c); // rows first, then the clk at the bottom
    expect(ys.slice(0, 3)).toEqual([FF_TOP, FF_TOP + 16, FF_TOP + 32]);
    // Nothing but the reset ever sits on the bottom edge now.
    const south = c.ports.filter((p) => p.layoutOptions["elk.port.side"] === "SOUTH");
    expect(south).toHaveLength(0);
  });

  it("orders enable rows below the data rows", () => {
    const en: SchPort = { id: 7, name: "gate", side: "west", role: "enable" };
    const c = ffChildOf([clk, en, data(2), data(3), q]);
    expect(byId(c, 7)!.y).toBeGreaterThan(byId(c, 2)!.y!);
    expect(byId(c, 7)!.y).toBeGreaterThan(byId(c, 3)!.y!);
  });

  it("keeps the clock wedge at the bottom of the west wall", () => {
    const c = ffChildOf([clk, data(2), q]);
    expect(byId(c, 1)!.y).toBe(c.height! - 11);
  });

  it("grows height so the input rows clear the clock wedge", () => {
    const many = Array.from({ length: 8 }, (_, i) => data(20 + i));
    const c = ffChildOf([clk, ...many, q]);
    expect(c.height).toBeGreaterThan(FF_H);
    const ys = westYs(c);
    // The last input row sits exactly at the top of the reserved wedge band
    // (the final west y is the clk itself at H - 11).
    expect(ys[ys.length - 2]).toBe(c.height! - FF_CLK_ZONE);
  });

  it("grows width for a labelled dangling output (#118)", () => {
    // A dangling Q has no wire label to name it, so the glyph labels it in-box
    // — the body must reserve room alongside the west rows.
    const dq: SchPort = { id: 31, name: "lane_state", side: "east", width: "[1:0]", dangling: true };
    expect(ffChildOf([clk, data(2), dq]).width).toBeGreaterThan(FF_W);
    // A wired Q still costs nothing (the wire label carries the name).
    expect(ffChildOf([clk, data(2), q]).width).toBe(FF_W);
  });

  it("grows width with the longest west pin label, not east names", () => {
    const wide: SchPort = { id: 30, name: "mem_la_wstrb", side: "west", width: "[31:0]" };
    expect(ffChildOf([clk, wide, q]).width).toBeGreaterThan(FF_W);
    // A long output name costs nothing — Q pins are unlabeled on the glyph
    // (the wire label already names the output).
    const longQ: SchPort = { id: 31, name: "cached_insn_opcode_wstrb", side: "east" };
    expect(ffChildOf([clk, data(2), longQ]).width).toBe(FF_W);
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

  it("keeps the reset on the south edge, dead-centre", () => {
    const rst: SchPort = { id: 8, name: "rst_n", side: "west" };
    const c = ffChildOf([clk, rst, data(2), q]);
    const south = c.ports.filter((p) => p.layoutOptions["elk.port.side"] === "SOUTH");
    expect(south).toHaveLength(1);
    expect(south[0].id).toBe(portId(8));
    expect(south[0].x).toBe(c.width! / 2);
  });
});

describe("storage child (latch)", () => {
  const latchOf = (ports: SchPort[]) =>
    toElk({
      root: "s",
      nodes: [{ id: 1, kind: "Latch", label: "lat", path: "s.l", expandable: false, ports }],
      edges: [],
    }).children[0];

  it("dispatches a latch to the FF-style storage child (#115)", () => {
    const c = latchOf([
      { id: 7, name: "gate", side: "west", role: "enable" },
      { id: 2, name: "d", side: "west" },
      { id: 9, name: "q", side: "east" },
    ]);
    expect(c.labels[0].text).toBe("LE");
    expect(c.layoutOptions["elk.portConstraints"]).toBe("FIXED_POS");
    // West label rows, not the generic instance box's 150px floor.
    expect(c.width).toBeLessThan(150);
    const west = c.ports.filter((p) => p.layoutOptions["elk.port.side"] === "WEST");
    expect(west).toHaveLength(2);
  });

  it("keeps the FF caption on FF nodes", () => {
    const c = ffChildOf([{ id: 2, name: "d", side: "west" }]);
    expect(c.labels[0].text).toBe("FF");
  });

  it("does not read FF clock/reset name conventions into latch pins", () => {
    // A level latch has no clock or async reset in the model — a data input
    // that merely *sounds* like one must stay a labelled west row, not become
    // a wedge or a south bubble.
    const c = latchOf([
      { id: 2, name: "rst_n", side: "west" },
      { id: 3, name: "clk_div", side: "west" },
      { id: 9, name: "q", side: "east" },
    ]);
    const rows = westYs(c);
    expect(rows).toEqual([FF_TOP, FF_TOP + 16]); // plain data rows, no wedge slot
    const south = c.ports.filter((p) => p.layoutOptions["elk.port.side"] === "SOUTH");
    expect(south).toHaveLength(0);
  });
});

describe("trunk groups (#117)", () => {
  // A bare interface's raw access port with member taps into one consumer box
  // on *both* walls (picorv32: valid/addr drive east, ready/rdata enter west),
  // plus an unrelated plain edge.
  const trunkGraph: SchematicGraph = {
    root: "s",
    nodes: [
      {
        id: 1,
        kind: "Instance",
        label: "core",
        path: "s.core",
        expandable: true,
        ports: [
          { id: 10, name: "mem_valid", side: "east" },
          { id: 11, name: "mem_addr", side: "east" },
          { id: 12, name: "mem_ready", side: "west" },
          { id: 14, name: "mem_rdata", side: "west" },
          { id: 13, name: "clk", side: "west" },
        ],
      },
      {
        id: 2,
        kind: "Interface",
        label: "bus",
        path: "s.bus",
        expandable: false,
        module: "mem_if",
        ports: [{ id: 900, name: "mem_if", side: "west", path: "s.bus", bundle: true }],
      },
    ],
    edges: [
      { id: 0, source: 10, target: 900, net: "bus.valid", net_path: "s.bus.valid" },
      { id: 1, source: 11, target: 900, net: "bus.addr", net_path: "s.bus.addr" },
      { id: 2, source: 900, target: 12, net: "bus.ready", net_path: "s.bus.ready" },
      { id: 4, source: 900, target: 14, net: "bus.rdata", net_path: "s.bus.rdata" },
      { id: 3, source: 13, target: 2, net: "clk", net_path: "s.clk" }, // plain box edge
    ],
  };

  it("groups a bundle port's member taps by consumer box and wall", () => {
    const groups = trunkGroups(trunkGraph);
    // One trunk per consumer *wall*: a single-sided gather bar cannot serve
    // pins on the opposite wall (their stubs would run under the box).
    expect(groups).toHaveLength(2);
    const east = groups.find((g) => g.side === "east")!;
    const west = groups.find((g) => g.side === "west")!;
    for (const g of [east, west]) {
      expect(g.port).toBe(900);
      expect(g.box).toBe(1);
      // Labelled by the bundle *instance* (unique per scope), not its interface
      // type — two same-typed bundles must not share a merged wire label.
      expect(g.name).toBe("bus");
      expect(g.path).toBe("s.bus");
    }
    expect(east.edges.map((e) => e.id).sort()).toEqual([0, 1]);
    expect(west.edges.map((e) => e.id).sort()).toEqual([2, 4]);
  });

  it("collapses each wall's group to one pin-anchored ELK trunk edge", () => {
    const elk = toElk(trunkGraph);
    const ids = elk.edges.map((e: any) => e.id);
    expect(ids).toContain("e0"); // east representative keeps the first member's id
    expect(ids).toContain("e2"); // west representative
    expect(ids).not.toContain("e1");
    expect(ids).not.toContain("e4");
    expect(ids).toContain("e3"); // unrelated edge untouched
    // Anchored at the representative member *pin* (not the bare box) so ELK
    // routes the trunk into the wall at pin height instead of an arbitrary
    // corner, and in the model's own orientation so layering stays truthful.
    const east = elk.edges.find((e: any) => e.id === "e0")!;
    expect(east.sources).toEqual([portId(10)]);
    expect(east.targets).toEqual([portId(900)]);
    // Labelled with the bundle instance's name, not one member's net.
    expect(east.labels?.[0]?.text).toBe("bus");
    const west = elk.edges.find((e: any) => e.id === "e2")!;
    expect(west.sources).toEqual([portId(900)]); // bundle drives these members
    expect(west.targets).toEqual([portId(12)]);
  });

  it("leaves singleton bundle connections alone", () => {
    const single: SchematicGraph = {
      ...trunkGraph,
      edges: [trunkGraph.edges[0], trunkGraph.edges[4]],
    };
    expect(trunkGroups(single)).toHaveLength(0);
    const elk = toElk(single);
    const e0 = elk.edges.find((e: any) => e.id === "e0")!;
    expect(e0.sources).toEqual([portId(10)]); // pin-anchored as before
  });

  it("computes the consumer-side gather bar geometry", () => {
    // Bar one stub-length off the pins' wall — *inside* ELK's 12px edge-node
    // channel, so it cannot lie on other edges' vertical runs. The trunk's own
    // final approach (at its representative pin's row) crosses the bar, so no
    // separate joint is needed.
    const east = gatherBar([{ x: 414, y: 204 }, { x: 414, y: 240 }], 1);
    expect(east.bar).toEqual([
      { x: 422, y: 204 },
      { x: 422, y: 240 },
    ]);
    expect(east.stubs).toEqual([
      [
        { x: 414, y: 204 },
        { x: 422, y: 204 },
      ],
      [
        { x: 414, y: 240 },
        { x: 422, y: 240 },
      ],
    ]);
    const west = gatherBar([{ x: 100, y: 50 }], -1);
    expect(west.bar).toEqual([
      { x: 92, y: 50 },
      { x: 92, y: 50 },
    ]);
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

// #263: the per-pan label placement is precomputed where it can be. These are the
// pure halves of that; `main.ts` holds the DOM and the rAF batching.
describe("segmentsAabb", () => {
  it("bounds every segment of a net", () => {
    expect(
      segmentsAabb([
        [
          { x: 10, y: 40 },
          { x: 50, y: 40 },
        ],
        [
          { x: 30, y: 5 },
          { x: 30, y: 90 },
        ],
      ]),
    ).toEqual({ x0: 10, y0: 5, x1: 50, y1: 90 });
  });

  it("bounds a single segment regardless of endpoint order", () => {
    expect(
      segmentsAabb([
        [
          { x: 90, y: 70 },
          { x: 20, y: 10 },
        ],
      ]),
    ).toEqual({ x0: 20, y0: 10, x1: 90, y1: 70 });
  });

  it("collapses a degenerate point segment to a zero-area box", () => {
    expect(
      segmentsAabb([
        [
          { x: 7, y: 7 },
          { x: 7, y: 7 },
        ],
      ]),
    ).toEqual({ x0: 7, y0: 7, x1: 7, y1: 7 });
  });

  it("returns null when there are no segments", () => {
    expect(segmentsAabb([])).toBeNull();
  });
});

describe("longestSegment", () => {
  it("measures by Manhattan length, not Euclidean", () => {
    // (0,0)-(10,0) is longer Euclidean (10 vs 8.49) but shorter Manhattan
    // (10 vs 12). `placeWireLabels` has always used Manhattan; picking the
    // Euclidean winner here would silently move every off-screen label.
    const diagonal: [Pt, Pt] = [
      { x: 0, y: 0 },
      { x: 6, y: 6 },
    ];
    expect(
      longestSegment([
        [
          { x: 0, y: 0 },
          { x: 10, y: 0 },
        ],
        diagonal,
      ]),
    ).toEqual(diagonal);
  });

  it("keeps the first of two equally long segments", () => {
    // Ties resolve first-wins (`>`, not `>=`), matching the original loop.
    const first: [Pt, Pt] = [
      { x: 0, y: 0 },
      { x: 20, y: 0 },
    ];
    expect(
      longestSegment([
        first,
        [
          { x: 0, y: 50 },
          { x: 20, y: 50 },
        ],
      ]),
    ).toEqual(first);
  });

  it("returns null when there are no segments", () => {
    expect(longestSegment([])).toBeNull();
  });
});

describe("rectsIntersect", () => {
  const view = { x0: 0, y0: 0, x1: 100, y1: 100 };

  it("reports an overlapping box", () => {
    expect(rectsIntersect(view, { x0: 80, y0: 80, x1: 200, y1: 200 })).toBe(true);
  });

  it("reports a fully contained box", () => {
    expect(rectsIntersect(view, { x0: 20, y0: 20, x1: 30, y1: 30 })).toBe(true);
  });

  it("counts a box touching the edge as intersecting", () => {
    // clampSegmentToRect keeps a segment lying exactly on the boundary, so the
    // cheap reject in front of it must not discard one.
    expect(rectsIntersect(view, { x0: 100, y0: 20, x1: 150, y1: 30 })).toBe(true);
  });

  it("rejects a disjoint box", () => {
    expect(rectsIntersect(view, { x0: 101, y0: 0, x1: 200, y1: 100 })).toBe(false);
  });
});

describe("placementsEqual", () => {
  const p: LabelPlacement = { x: 10, y: 20, rotate: 0, anchor: "middle", baseline: "auto" };

  it("treats an identical placement as unchanged", () => {
    expect(placementsEqual({ ...p }, { ...p })).toBe(true);
  });

  it("reports a moved label as changed", () => {
    expect(placementsEqual(p, { ...p, x: 11 })).toBe(false);
  });

  it("reports a rotated label as changed", () => {
    expect(placementsEqual(p, { ...p, rotate: 90 })).toBe(false);
  });

  it("treats a never-placed label as changed", () => {
    expect(placementsEqual(null, p)).toBe(false);
  });
});

describe("chooseLabelSegment", () => {
  // The two-pass loop `placeWireLabels` ran before #263, kept verbatim as the
  // oracle: the precomputed path must agree with it exactly, not merely closely.
  const reference = (segs: [Pt, Pt][], view: VRect): LabelPlacement | null => {
    const manhattan = (a: Pt, b: Pt) => Math.abs(a.x - b.x) + Math.abs(a.y - b.y);
    let best: [Pt, Pt] | null = null;
    let bestLen = -1;
    for (const [a, b] of segs) {
      const vis = clampSegmentToRect(a, b, view);
      if (!vis) continue;
      const len = manhattan(vis[0], vis[1]);
      if (len > bestLen) [bestLen, best] = [len, vis];
    }
    if (!best) {
      for (const [a, b] of segs) {
        const len = manhattan(a, b);
        if (len > bestLen) [bestLen, best] = [len, [a, b]];
      }
    }
    return best ? wireLabelPlacement(best[0], best[1]) : null;
  };

  const net: [Pt, Pt][] = [
    [
      { x: 0, y: 10 },
      { x: 120, y: 10 },
    ],
    [
      { x: 120, y: 10 },
      { x: 120, y: 240 },
    ],
    [
      { x: 120, y: 240 },
      { x: 300, y: 240 },
    ],
  ];

  const views: [string, VRect][] = [
    ["the whole net visible", { x0: -10, y0: -10, x1: 400, y1: 400 }],
    ["one segment partly visible", { x0: 50, y0: 0, x1: 200, y1: 100 }],
    ["a corner clipping two segments", { x0: 100, y0: 200, x1: 400, y1: 300 }],
    ["the net entirely off-screen", { x0: 500, y0: 500, x1: 900, y1: 900 }],
  ];

  for (const [name, view] of views) {
    it(`matches the original loop with ${name}`, () => {
      expect(chooseLabelSegment(net, view, labelGeometry(net))).toEqual(reference(net, view));
    });
  }

  it("parks an off-screen net on its longest segment", () => {
    // The explicit statement of the fallback, so the oracle above cannot drift
    // in step with a bug: the longest run is the 230-unit vertical one.
    const view = { x0: 500, y0: 500, x1: 900, y1: 900 };
    expect(chooseLabelSegment(net, view, labelGeometry(net))).toEqual(
      wireLabelPlacement({ x: 120, y: 10 }, { x: 120, y: 240 }),
    );
  });

  it("returns null for a label with no segments", () => {
    expect(chooseLabelSegment([], { x0: 0, y0: 0, x1: 10, y1: 10 }, labelGeometry([]))).toBeNull();
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

  it("magnifies a small graph to fill the pane when maxZoom allows (#114)", () => {
    // 200x150 graph in an 800x600 pane: both ratios are 4, under the 6x cap.
    expect(fitZoom(200, 150, 800, 600, 6)).toBeCloseTo(4);
  });

  it("caps magnification at maxZoom", () => {
    // Tiny graph in a huge pane: fill would be 60x — clamp to the cap.
    expect(fitZoom(100, 100, 6000, 6000, 6)).toBe(6);
  });

  it("still shrinks past 1 when maxZoom is raised", () => {
    // maxZoom only lifts the ceiling; a large graph still scales down to fit.
    expect(fitZoom(2000, 1500, 800, 600, 6)).toBeCloseTo(0.4);
  });

  it("ignores maxZoom when the pane is unmeasurable", () => {
    expect(fitZoom(200, 150, 0, 0, 6)).toBe(1);
  });
});

// --- gate-level primitives (#157) ------------------------------------------
const primOf = (kind: string, ports: SchPort[]) =>
  toElk({
    root: "s",
    nodes: [{ id: 1, kind, label: kind, path: "s.g", expandable: false, ports }],
    edges: [],
  }).children[0];

const sided = (c: any, side: string): any[] =>
  c.ports.filter((p: any) => p.layoutOptions["elk.port.side"] === side);

describe("gate child (#157)", () => {
  it("classifies the 13 boolean/datapath kinds but not Mux", () => {
    for (const k of ["And", "Or", "Xor", "Xnor", "Nand", "Nor", "Not", "Buf", "Add", "Sub", "Mul", "Cmp", "Shift"]) {
      expect(isGateKind(k)).toBe(true);
    }
    expect(isGateKind("Mux")).toBe(false);
    expect(isGateKind("Comb")).toBe(false);
  });

  it("sizes a 2-input gate to the floor with inputs west, result east, FIXED_POS", () => {
    const c = primOf("And", [
      { id: 10, name: "a", side: "west", path: "s.a" },
      { id: 11, name: "b", side: "west", path: "s.b" },
      { id: 12, name: "", side: "east", path: "s.g" },
    ]);
    expect(c.width).toBe(GATE_W);
    expect(c.height).toBe(GATE_H);
    expect(c.layoutOptions["elk.portConstraints"]).toBe("FIXED_POS");
    expect(sided(c, "WEST").length).toBe(2);
    const east = sided(c, "EAST");
    expect(east.length).toBe(1);
    expect(east[0].x).toBe(c.width);
    expect(east[0].y).toBe(c.height / 2);
  });

  it("widens a datapath box to fit its operator label", () => {
    const c = primOf("Cmp", [
      { id: 10, name: "a", side: "west", path: "s.a" },
      { id: 12, name: "", side: "east", path: "s.g" },
    ]);
    // Label "Cmp" (len 3) is short, so it stays at the floor width here — the
    // point is the sizer never goes *below* the floor.
    expect(c.width).toBeGreaterThanOrEqual(GATE_W);
  });
});

describe("mux child (#157)", () => {
  const muxPorts: SchPort[] = [
    { id: 10, name: "d0", side: "west", path: "s.a" },
    { id: 11, name: "d1", side: "west", path: "s.b" },
    { id: 12, name: "sel", side: "west", path: "s.sel", role: "sel" },
    { id: 13, name: "", side: "east", path: "s.g" },
  ];

  it("puts the select pin on the SOUTH wall, data west, result east", () => {
    const c = primOf("Mux", muxPorts);
    expect(c.width).toBe(MUX_W);
    expect(c.height).toBe(MUX_H);
    expect(c.layoutOptions["elk.portConstraints"]).toBe("FIXED_POS");
    // Two data branches on the west wall, not the sel pin.
    expect(sided(c, "WEST").length).toBe(2);
    const south = sided(c, "SOUTH");
    expect(south.length).toBe(1);
    expect(south[0].id).toBe(portId(12));
    expect(south[0].x).toBe(c.width / 2);
    expect(south[0].y).toBe(c.height);
    const east = sided(c, "EAST");
    expect(east.length).toBe(1);
    expect(east[0].y).toBe(c.height / 2);
  });
});

// Trace-mode affordance gutter (#244 PR4). The ◀/▶ expand controls and the "+N
// more" badge are drawn outboard of a box wall, so ELK has to be told to keep the
// neighbouring layer clear — otherwise a control lands on the box next door.
describe("affordance margins", () => {
  const box = (ports: SchPort[]): SchematicGraph => ({
    root: "s",
    nodes: [
      {
        id: 1,
        kind: "Instance",
        label: "u",
        path: "s.u",
        expandable: true,
        ports,
      },
    ],
    edges: [],
  });
  const pin = (id: number, side: "west" | "east", more?: number): SchPort => ({
    id,
    name: `p${id}`,
    side,
    path: `s.u.p${id}`,
    ...(more === undefined ? {} : { more }),
  });
  const margins = (g: SchematicGraph, affordances: boolean) =>
    toElk(g, { affordances }).children[0].layoutOptions["elk.margins"];
  // `[0-9.]` rather than `\d`: this pattern is built from a template literal, and
  // `\d` there is not a regex escape — it collapses to a plain `d`, silently
  // matching nothing.
  const side = (m: string | undefined, key: string) =>
    Number(new RegExp(key + "=([0-9.]+)").exec(m ?? "")?.[1] ?? 0);

  it("reserves nothing when affordances are off", () => {
    // The hierarchy view draws no controls, so its spacing must be untouched —
    // this is what keeps PR4 invisible to every existing scope graph.
    expect(margins(box([pin(2, "west"), pin(3, "east")]), false)).toBeUndefined();
  });

  it("reserves a gutter on each wall that has pins", () => {
    const m = margins(box([pin(2, "west"), pin(3, "east")]), true);
    expect(side(m, "left")).toBeGreaterThan(0);
    expect(side(m, "right")).toBeGreaterThan(0);
  });

  it("reserves only the walls that actually have pins", () => {
    const m = margins(box([pin(2, "west")]), true);
    expect(side(m, "left")).toBeGreaterThan(0);
    expect(side(m, "right")).toBe(0);
  });

  it("widens a wall that carries a `+N` badge", () => {
    // The badge is text, so it needs more room than the bare control.
    const plain = margins(box([pin(2, "east")]), true);
    const badged = margins(box([pin(2, "east", 128)]), true);
    expect(side(badged, "right")).toBeGreaterThan(side(plain, "right"));
  });

  it("adds to a const-label margin instead of clobbering it", () => {
    // A gate reserves its own west margin for inline tie values (#199). Both
    // reservations are real, so the gutter must stack on top rather than replace
    // it — which a plain overwrite would do silently.
    const gate: SchematicGraph = {
      root: "s",
      nodes: [
        {
          id: 1,
          kind: "And",
          label: "&",
          path: "s.g",
          expandable: false,
          ports: [
            { id: 2, name: "a", side: "west", path: "s.a", constant: "8'hFF" },
            { id: 3, name: "y", side: "east", path: "s.y" },
          ],
        },
      ],
      edges: [],
    };
    const withConstOnly = side(margins(gate, false), "left");
    expect(withConstOnly).toBeGreaterThan(0); // the #199 reservation still fires
    expect(side(margins(gate, true), "left")).toBeGreaterThan(withConstOnly);
  });
});
