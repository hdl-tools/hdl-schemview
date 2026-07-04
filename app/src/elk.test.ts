import { describe, it, expect } from "vitest";
import {
  toElk,
  nodeId,
  portId,
  fitZoom,
  ffRole,
  FF_W,
  FF_H,
  FF_TOP,
  FF_CLK_ZONE,
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
    // #106: inside the consumer, its modport-qualified interface port is the
    // module's window to the outside — it hugs the boundary, but in the first
    // *regular* layer (FIRST/LAST, not the _SEPARATE frame column) so the
    // scope's own boundary pins keep their exclusive frame layer and stay at
    // the top corner instead of stacking under the tall bundle. A mostly-in
    // view (pins mirrored east, feeding the design) sits first; a mostly-out
    // view last. A bare interface instance (no modport) stays a free box.
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
    expect(mostlyIn.layoutOptions["elk.layered.layering.layerConstraint"]).toBe("FIRST");
    const mostlyOut = toElk(bundle("core", ["west", "west", "east"])).children[0];
    expect(mostlyOut.layoutOptions["elk.layered.layering.layerConstraint"]).toBe("LAST");
    const bare = toElk(bundle(undefined, ["east", "west"])).children[0];
    expect(bare.layoutOptions["elk.layered.layering.layerConstraint"]).toBeUndefined();
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
