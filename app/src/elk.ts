// Adapter from our SchematicGraph to an ELK graph, plus a layout helper.
// `toElk` is pure (unit-tested); `layout` runs elkjs.
import ELK from "elkjs/lib/elk.bundled.js";
import type { SchEdge, SchematicGraph, SchNode, SchPort } from "./types";

export interface ElkLabel {
  text: string;
  width?: number;
  height?: number;
  x?: number;
  y?: number;
}
export interface ElkPort {
  id: string;
  width: number;
  height: number;
  layoutOptions: Record<string, string>;
  x?: number;
  y?: number;
}
export interface ElkChild {
  id: string;
  width: number;
  height: number;
  labels: ElkLabel[];
  ports: ElkPort[];
  layoutOptions: Record<string, string>;
  x?: number;
  y?: number;
}
export interface ElkEdge {
  id: string;
  sources: string[];
  targets: string[];
  labels?: ElkLabel[];
}
export interface ElkGraph {
  id: string;
  layoutOptions: Record<string, string>;
  children: ElkChild[];
  edges: ElkEdge[];
}

export const nodeId = (id: number) => `n${id}`;
export const portId = (id: number) => `p${id}`;

// Rough character widths (px) for the fonts used in the schematic, so boxes are
// sized to fit their (now visible) pin and title text without overlap.
const PIN_CH = 6.2; // port-name + width label
const TITLE_CH = 7.5; // instance / (module) title
const ROW_H = 18; // vertical space per pin row
const PIN_HALF = 4; // pin triangle half-height (it extends ±4 around its py)

const pinLabelLen = (p: SchPort) => p.name.length + (p.width ? p.width.length + 1 : 0);

// Pointed-cap size of the interface bundle hexagon (top/bottom apexes). Shared
// with the renderer so layout and glyph agree on the straight-wall zone.
export const IFACE_CAP = 12;

// Gate-level projection primitives (#157) — the 13 boolean/datapath gate kinds a
// dissolved combinational block surfaces. `Mux` is deliberately excluded: it has
// its own trapezoid sizer/glyph, so callers dispatch it separately.
const GATE_KINDS = new Set([
  "And",
  "Or",
  "Xor",
  "Xnor",
  "Nand",
  "Nor",
  "Not",
  "Buf",
  "Add",
  "Sub",
  "Mul",
  "Cmp",
  "Shift",
  "Concat",
]);
export const isGateKind = (k: string): boolean => GATE_KINDS.has(k);

// Process-level logic nodes (combinational process / level latch / continuous
// assign) plus the gate-level primitives (#157). They draw bare pin stubs and
// size compactly — distinct from module instances. (FF/Latch/Assign/Memory and
// every gate dispatch to their own symbol before the generic path, so only Comb
// still reaches the call sites that use this predicate — kept complete so it
// states the kind taxonomy and colours a gate as logic on any generic path.)
export const isLogicKind = (k: string): boolean =>
  k === "Comb" || k === "Latch" || k === "Assign" || k === "Mux" || isGateKind(k);

// --- inferred FF symbol ----------------------------------------------------
export type FfRole = "clk" | "reset" | "enable" | "q" | "data";
// Classify an FF/latch pin: Q on the east, then the model role fact (#59 — the
// harness tags the clock, async-reset and latch-enable structurally), then
// conventional names as a fallback for facts the model cannot state (a sync
// reset or an FF clock-enable is indistinguishable from data structurally).
// The clock/reset conventions are FF-only: a level latch has neither concept
// in the model, so a latch input merely *named* `clk_div`/`rst_n` stays data.
export function ffRole(p: SchPort, kind: string = "FF"): FfRole {
  if (p.side === "east") return "q";
  if (p.role === "clk" || p.role === "reset" || p.role === "enable") return p.role;
  if (kind === "FF") {
    if (/(^|_)(clk|clock)/i.test(p.name)) return "clk";
    if (/(^|_)(rst|reset)/i.test(p.name)) return "reset";
  }
  if (/(^|_)(en|ce|enable)($|_)/i.test(p.name)) return "enable";
  return "data";
}
export const FF_H = 46;
export const FF_W = 56; // default FF box width
const FF_PITCH = 16; // vertical pitch between adjacent west input rows
export const FF_TOP = 10; // top inset of the west input column
// Bottom band of the west wall reserved for the clock wedge (centred at H - 11,
// spanning H-17..H-5): the last input row stops FF_CLK_ZONE above the bottom so
// its pin triangle clears the wedge.
export const FF_CLK_ZONE = 24;
export const FF_LABEL_PAD = 11; // gap from the wall to a pin label (renderer draws at this x)
const FF_EAST_GAP = 20; // room for the Q triangle (depth 8) + clearance
const FF_Q_PITCH = 16; // minimum spacing between adjacent east (output) pins
const FF_Q_TOP = 12; // top inset of the east output column
const FF_Q_BOT = 12; // bottom inset of the east output column

// A storage element (inferred FF or level latch, #115): data + enable inputs as
// labelled rows down the west wall (enable last, just above the clock wedge),
// the async reset as a bubble on the south edge, and one east pin per distinct
// output spread down the right wall — so a register driving several signals shows
// each as its own output rather than one fanned-out pin. Width fits the longest
// input label; height fits the input rows plus wedge, or the output column.
// FIXED_POS so the renderer can match glyphs to ports.
function storageChild(n: SchNode): ElkChild {
  const by = (r: FfRole) => n.ports.filter((p) => ffRole(p, n.kind) === r);
  const westPins = [...by("data"), ...by("enable")];
  const reset = by("reset");
  const clks = by("clk");
  const qs = by("q");
  const rows = westPins.length;

  // The longest west label must fit between the label pad and the Q triangles.
  // A dangling Q is labelled in-box too (#118) — no wire label names it — so
  // reserve room for the longest such label alongside the west rows.
  const wMax = westPins.reduce((m, p) => Math.max(m, pinLabelLen(p)), 0);
  const eMax = qs.reduce((m, p) => (p.dangling ? Math.max(m, pinLabelLen(p)) : m), 0);
  const W = Math.max(
    FF_W,
    FF_LABEL_PAD + (wMax + eMax) * PIN_CH + (eMax ? FF_LABEL_PAD : 0) + FF_EAST_GAP,
  );
  // A latch has no wedge, so its last row only needs the plain top inset below it.
  const bot = clks.length ? FF_CLK_ZONE : FF_TOP;
  const H = Math.max(
    FF_H,
    rows ? FF_TOP + (rows - 1) * FF_PITCH + bot : 0,
    // Tall enough to spread every output down the east wall at >= FF_Q_PITCH.
    qs.length > 1 ? FF_Q_TOP + (qs.length - 1) * FF_Q_PITCH + FF_Q_BOT : 0,
  );
  const west = (id: number, y: number): ElkPort => ({
    id: portId(id),
    width: 0,
    height: 0,
    x: 0,
    y,
    layoutOptions: { "elk.port.side": "WEST" },
  });
  const ports: ElkPort[] = [];
  // Outputs: one east pin each, spread down the right wall (a single output stays
  // centred, matching the classic FF look).
  const span = H - FF_Q_TOP - FF_Q_BOT;
  qs.forEach((p, i) =>
    ports.push({
      id: portId(p.id),
      width: 0,
      height: 0,
      x: W,
      y: qs.length > 1 ? FF_Q_TOP + (span * i) / (qs.length - 1) : H / 2,
      layoutOptions: { "elk.port.side": "EAST" },
    }),
  );
  westPins.forEach((p, i) => ports.push(west(p.id, FF_TOP + i * FF_PITCH)));
  for (const p of clks) ports.push(west(p.id, H - 11));
  // The async reset draws as a bubble under the box, dead-centre.
  if (reset.length)
    ports.push({
      id: portId(reset[0].id),
      width: 0,
      height: 0,
      x: W / 2,
      y: H,
      layoutOptions: { "elk.port.side": "SOUTH" },
    });

  return {
    id: nodeId(n.id),
    width: W,
    height: H,
    labels: [{ text: n.kind === "Latch" ? "LE" : "FF" }],
    layoutOptions: { "elk.portConstraints": "FIXED_POS" },
    ports,
  };
}

// A continuous assign, drawn as a small anonymous square (#135): inputs spread
// down the west wall, the single output leaves the east wall centre. No label —
// the wires' net labels carry the meaning; the square only marks "a function
// happens here". Height grows a couple px per input so heavy fan-in reads.
function assignChild(n: SchNode): ElkChild {
  const west = n.ports.filter((p) => p.side !== "east");
  const east = n.ports.filter((p) => p.side === "east");
  const rows = Math.max(west.length, 1);
  const w = 16;
  const h = Math.max(16, 4 + rows * 6);
  const ports: ElkPort[] = [];
  const top = 4;
  const span = Math.max(0, h - 2 * top);
  west.forEach((p, i) => {
    ports.push({
      id: portId(p.id),
      width: 0,
      height: 0,
      x: 0,
      y: west.length > 1 ? top + (span * i) / (west.length - 1) : h / 2,
      layoutOptions: { "elk.port.side": "WEST" },
    });
  });
  // The assigned LHS — normally one output, centred on the east wall.
  east.forEach((p) =>
    ports.push({
      id: portId(p.id),
      width: 0,
      height: 0,
      x: w,
      y: h / 2,
      layoutOptions: { "elk.port.side": "EAST" },
    }),
  );
  return {
    id: nodeId(n.id),
    width: w,
    height: h,
    labels: [],
    layoutOptions: { "elk.portConstraints": "FIXED_POS" },
    ports,
  };
}

// --- gate-level primitives (#157) ------------------------------------------
export const GATE_W = 52; // default gate box width (west lead zone + body; grows for a datapath op label)
export const GATE_H = 40; // default gate box height
const GATE_TOP = 8; // top/bottom inset of the west input column
const GATE_PITCH = 12; // vertical pitch between adjacent west input rows
export const MUX_W = 30; // mux trapezoid width (fixed — the glyph is narrow)
export const MUX_H = 44; // default mux trapezoid height
const MUX_TOP = 8; // top/bottom inset of the west data-input column
const MUX_PITCH = 14; // vertical pitch between adjacent data inputs

// A boolean/datapath gate (And/Or/…/Cmp/Shift): operand inputs spread down the
// west wall, the single result leaves the east wall centre — like an assign, but
// wider so the IEEE distinctive glyph (or a datapath op label) has room. The
// renderer picks the shape from `kind`; layout only needs a fixed box + walled
// ports. FIXED_POS so the renderer's glyph lines up with the ports.
function gateChild(n: SchNode): ElkChild {
  const west = n.ports.filter((p) => p.side !== "east");
  const east = n.ports.filter((p) => p.side === "east");
  const rows = Math.max(west.length, 1);
  // Datapath boxes (Add/Cmp/Shift/…) show the operator label; boolean gates draw
  // a bare glyph, so only widen when there is a label to fit.
  const w = Math.max(GATE_W, n.label.length * TITLE_CH + 16);
  const h = Math.max(GATE_H, 2 * GATE_TOP + (rows - 1) * GATE_PITCH);
  const span = Math.max(0, h - 2 * GATE_TOP);
  // Reserve a west margin for inline const/param tie values (#199), so ELK keeps
  // the neighbouring layer clear of the value text drawn left of the wall.
  const constLen = west.reduce((m, p) => Math.max(m, p.constant?.length ?? 0), 0);
  const leftMargin = constLen ? constLen * 5.6 + 6 : 0;
  const ports: ElkPort[] = [];
  west.forEach((p, i) =>
    ports.push({
      id: portId(p.id),
      width: 0,
      height: 0,
      x: 0,
      y: west.length > 1 ? GATE_TOP + (span * i) / (west.length - 1) : h / 2,
      layoutOptions: { "elk.port.side": "WEST" },
    }),
  );
  east.forEach((p) =>
    ports.push({
      id: portId(p.id),
      width: 0,
      height: 0,
      x: w,
      y: h / 2,
      layoutOptions: { "elk.port.side": "EAST" },
    }),
  );
  return {
    id: nodeId(n.id),
    width: w,
    height: h,
    labels: [],
    layoutOptions: {
      "elk.portConstraints": "FIXED_POS",
      ...(leftMargin
        ? { "elk.margins": `[left=${leftMargin.toFixed(1)},top=0.0,right=0.0,bottom=0.0]` }
        : {}),
    },
    ports,
  };
}

// A gate-level multiplexer (#157): data-branch inputs spread down the west wall,
// the single result on the east wall centre, and the select input on the SOUTH
// wall (the `role === "sel"` pin, from the model's MuxPort::Sel) so the renderer
// can place it under the trapezoid — mirroring the FF reset-bubble placement.
function muxChild(n: SchNode): ElkChild {
  const sel = n.ports.filter((p) => p.role === "sel");
  const data = n.ports.filter((p) => p.side !== "east" && p.role !== "sel");
  const east = n.ports.filter((p) => p.side === "east");
  const rows = Math.max(data.length, 1);
  const w = MUX_W;
  const h = Math.max(MUX_H, 2 * MUX_TOP + (rows - 1) * MUX_PITCH);
  const span = Math.max(0, h - 2 * MUX_TOP);
  // West margin for inline const/param data-branch tie values (#199).
  const constLen = data.reduce((m, p) => Math.max(m, p.constant?.length ?? 0), 0);
  const leftMargin = constLen ? constLen * 5.6 + 6 : 0;
  const ports: ElkPort[] = [];
  data.forEach((p, i) =>
    ports.push({
      id: portId(p.id),
      width: 0,
      height: 0,
      x: 0,
      y: data.length > 1 ? MUX_TOP + (span * i) / (data.length - 1) : h / 2,
      layoutOptions: { "elk.port.side": "WEST" },
    }),
  );
  east.forEach((p) =>
    ports.push({
      id: portId(p.id),
      width: 0,
      height: 0,
      x: w,
      y: h / 2,
      layoutOptions: { "elk.port.side": "EAST" },
    }),
  );
  for (const p of sel)
    ports.push({
      id: portId(p.id),
      width: 0,
      height: 0,
      x: w / 2,
      y: h,
      layoutOptions: { "elk.port.side": "SOUTH" },
    });
  return {
    id: nodeId(n.id),
    width: w,
    height: h,
    labels: [],
    layoutOptions: {
      "elk.portConstraints": "FIXED_POS",
      ...(leftMargin
        ? { "elk.margins": `[left=${leftMargin.toFixed(1)},top=0.0,right=0.0,bottom=0.0]` }
        : {}),
    },
    ports,
  };
}

// --- memory array (#112) ---------------------------------------------------
export const MEM_W = 84; // default memory-array box width
export const MEM_H = 56; // default memory-array box height
export const MEM_LABEL_PAD = 11; // gap from the wall to a pin (role) label
const MEM_TOP = 16; // top band reserved for the name + depth sublabel
const MEM_ROW = 18; // vertical pitch between pin rows
const MEM_BOT = 10; // bottom inset below the last pin row

// A memory array: addr/din inputs down the west wall, dout output(s) on the east
// wall, sized to fit the pin (role) labels and the title/depth band. The array
// motif (stacked back-cards, word-row dividers, INIT badge) is decoration drawn
// by the renderer; layout only needs the sized box + walled ports. FIXED_POS so
// the renderer's glyphs line up with the ports.
function memoryChild(n: SchNode): ElkChild {
  const west = n.ports.filter((p) => p.side !== "east");
  const east = n.ports.filter((p) => p.side === "east");
  // A memory pin is labelled by its role (addr/din/dout), not the signal name —
  // the wire label already names the net, and the role reads as a RAM port.
  const roleLen = (p: SchPort) => (p.role ?? p.name).length + (p.width ? p.width.length + 1 : 0);
  const wMax = west.reduce((m, p) => Math.max(m, roleLen(p)), 0);
  const eMax = east.reduce((m, p) => Math.max(m, roleLen(p)), 0);
  const rows = Math.max(west.length, east.length, 1);
  const titleLen = Math.max(n.label.length, n.memDepth ? String(n.memDepth).length + 4 : 0);
  const W = Math.max(
    MEM_W,
    titleLen * TITLE_CH + 20,
    MEM_LABEL_PAD * 2 + (wMax + eMax) * PIN_CH + 20,
  );
  const H = Math.max(MEM_H, MEM_TOP + rows * MEM_ROW + MEM_BOT);
  const span = H - MEM_TOP - MEM_BOT;
  const sideY = (i: number, len: number) => (len > 1 ? MEM_TOP + (span * i) / (len - 1) : H / 2);
  const ports: ElkPort[] = [];
  west.forEach((p, i) =>
    ports.push({
      id: portId(p.id),
      width: 0,
      height: 0,
      x: 0,
      y: sideY(i, west.length),
      layoutOptions: { "elk.port.side": "WEST" },
    }),
  );
  east.forEach((p, i) =>
    ports.push({
      id: portId(p.id),
      width: 0,
      height: 0,
      x: W,
      y: sideY(i, east.length),
      layoutOptions: { "elk.port.side": "EAST" },
    }),
  );
  return {
    id: nodeId(n.id),
    width: W,
    height: H,
    labels: [{ text: n.label }],
    layoutOptions: { "elk.portConstraints": "FIXED_POS" },
    ports,
  };
}

/// Pure mapping: SchematicGraph -> ELK graph (no geometry yet).
///
/// Ports are given zero width/height on purpose: with a sized port ELK anchors
/// the edge to the port box's *outer* edge (≈ the port size outside the wall) and
/// vertical centre, which left wires floating ~8px off the pins. A zero-size port
/// makes ELK route the edge to exactly the `(x, y)` we set — the box wall, where
/// the renderer draws the pin — so wires meet their pins.
/// Layout knobs that are a property of the *view*, not of the graph (#244 PR4).
export interface LayoutOpts {
  /// Reserve an outboard gutter on each wall for trace mode's ◀/▶ expand controls
  /// and its "+N more" badge. Off for the hierarchy view, which draws neither.
  affordances?: boolean;
}

/// Width of the outboard band a trace affordance occupies, per wall.
///
/// Reserved as a whole-node `elk.margins` — the #199 const-label precedent — so
/// ELK keeps the neighbouring layer clear instead of letting a control land on a
/// box next door. Outboard rather than inboard because the inward band is already
/// spoken for by the pin triangle and its signal-name label.
export const AFFORD_GUTTER = 16;

/// Extra room for the widest "+N" badge on one wall, on top of the gutter. Sized
/// like the const-label (5.6 px/char at 9 px), since it is the same kind of small
/// outboard text.
function badgeWidth(ports: SchPort[]): number {
  const widest = ports.reduce((m, p) => Math.max(m, p.more ? `+${p.more}`.length : 0), 0);
  return widest ? widest * 5.6 + 4 : 0;
}

/// Merge a trace affordance gutter into a child's `elk.margins`.
///
/// Applied once, here, rather than threaded through all six per-kind sizers: it is
/// the same reservation for every box shape, and the sizers already disagree about
/// whether they emit margins at all (only gate/mux do, for #199). Parses any left
/// margin they set so the two reservations add instead of clobbering each other.
function withAffordanceMargins(child: ElkChild, n: SchNode, on: boolean): ElkChild {
  if (!on) return child;
  const west = n.ports.filter((p) => p.side !== "east");
  const east = n.ports.filter((p) => p.side === "east");
  if (!west.length && !east.length) return child;
  const prev = child.layoutOptions["elk.margins"];
  const prevLeft = prev ? Number(/left=([\d.]+)/.exec(prev)?.[1] ?? 0) : 0;
  const left = (west.length ? AFFORD_GUTTER : 0) + badgeWidth(west) + prevLeft;
  const right = (east.length ? AFFORD_GUTTER : 0) + badgeWidth(east);
  return {
    ...child,
    layoutOptions: {
      ...child.layoutOptions,
      "elk.margins": `[left=${left.toFixed(1)},top=0.0,right=${right.toFixed(1)},bottom=0.0]`,
    },
  };
}

export function toElk(graph: SchematicGraph, opts: LayoutOpts = {}): ElkGraph {
  const portOwner = new Set<number>();
  for (const n of graph.nodes) for (const p of n.ports) portOwner.add(p.id);
  // Edge-connected port/node ids — a modport bundle pin only fans out its
  // wired members (an unconnected member has no in-scope semantics; the full
  // membership stays visible on the interface instance's hexagon).
  const wired = new Set<number>();
  for (const e of graph.edges) {
    wired.add(e.source);
    wired.add(e.target);
  }

  const children: ElkChild[] = graph.nodes.map((n): ElkChild =>
    withAffordanceMargins(sizeChild(n), n, opts.affordances === true),
  );

  function sizeChild(n: SchNode): ElkChild {
    // Inferred storage (register / level latch): an FF-style symbol with
    // labelled west input rows, clock wedge, south reset bubble, east Qs.
    if (n.kind === "FF" || n.kind === "Latch") return storageChild(n);
    // Continuous assign: a stadium capsule (inputs west, output east).
    if (n.kind === "Assign") return assignChild(n);
    // Memory array (#112): an array box, addr/din west, dout east.
    if (n.kind === "Memory") return memoryChild(n);
    // Gate-level primitives (#157): a mux trapezoid (select on the south wall) or
    // a boolean/datapath gate box (operands west, result east).
    if (n.kind === "Mux") return muxChild(n);
    if (isGateKind(n.kind)) return gateChild(n);
    // Boundary I/O pin: a small node sized to its label, with its single port
    // already sided toward the design.
    if (n.kind === "Port") {
      const lab = n.ports[0] ? pinLabelLen(n.ports[0]) : n.label.length;
      // Boundary I/O pins cluster at the frame (first/last column); a constant
      // tie-off instead sits in the layer just left of the box it drives.
      const input = n.ports[0]?.side === "east";
      const layoutOptions: Record<string, string> = { "elk.portConstraints": "FIXED_SIDE" };
      if (!n.constant) {
        layoutOptions["elk.layered.layering.layerConstraint"] = input
          ? "FIRST_SEPARATE"
          : "LAST_SEPARATE";
      }
      return {
        id: nodeId(n.id),
        width: Math.max(40, lab * PIN_CH + 24),
        height: 26,
        labels: [{ text: n.label }],
        layoutOptions,
        ports: n.ports.map((p) => ({
          id: portId(p.id),
          width: 0,
          height: 0,
          layoutOptions: { "elk.port.side": p.side === "east" ? "EAST" : "WEST" },
        })),
      };
    }
    // #125: a modport-qualified bundle is the scope's window to the outside —
    // an interface *port*, not an instance — so it draws as a square frame pin
    // rather than the hexagon box, sharing the _SEPARATE frame column with the
    // scope's own boundary pins so the squares and triangles line up. (ELK
    // reserves _SEPARATE layers for external-port dummies and calls
    // mixed-direction edges there unsupported; elkjs 0.9.3 routes them fine —
    // verified by the layout smoke test.) A mostly-in view feeds the design
    // from its east side, so it sits first; a mostly-out view last. Only wired
    // members carry ports, every one anchored at the square glyph's wall
    // centre so the member wires visually meet the pin; unconnected members
    // are omitted — they have no in-scope semantics.
    if (n.kind === "Interface" && n.modport) {
      const eastCount = n.ports.filter((p) => p.side === "east").length;
      const first = eastCount >= n.ports.length - eastCount;
      const members = n.ports.filter((p) => wired.has(p.id));
      const sub = `(${n.module ?? ""}.${n.modport})`;
      const w = Math.max(40, Math.max(n.label.length, sub.length) * PIN_CH + 24);
      const h = 26;
      return {
        id: nodeId(n.id),
        width: w,
        height: h,
        labels: [{ text: n.label }],
        layoutOptions: {
          "elk.portConstraints": "FIXED_POS",
          "elk.layered.layering.layerConstraint": first ? "FIRST_SEPARATE" : "LAST_SEPARATE",
        },
        ports: members.map((p) => ({
          id: portId(p.id),
          width: 0,
          height: 0,
          x: first ? w : 0,
          y: h / 2,
          layoutOptions: { "elk.port.side": first ? "EAST" : "WEST" },
        })),
      };
    }
    const west = n.ports.filter((p) => p.side !== "east");
    const east = n.ports.filter((p) => p.side === "east");
    const titleLen = Math.max(n.label.length, n.module ? n.module.length + 2 : 0);
    // Comb/assign nodes draw bare pin stubs (no per-pin labels), so size them
    // compactly from the title alone. The generic box instead reserves room for the
    // west+east pin labels side by side — sizing a label-less logic node that way
    // would widen it to fit signal names that are never drawn.
    const isLogic = isLogicKind(n.kind);
    const wMax = west.reduce((m, p) => Math.max(m, pinLabelLen(p)), 0);
    const eMax = east.reduce((m, p) => Math.max(m, pinLabelLen(p)), 0);
    const w = isLogic
      ? Math.max(64, titleLen * TITLE_CH + 24)
      : Math.max(150, titleLen * TITLE_CH + 28, (wMax + eMax) * PIN_CH + 56);
    const rows = Math.max(west.length, east.length, 1);
    // Tall enough for the two-line title band plus one row per pin. An
    // interface bundle draws pointed hexagon caps top and bottom, so it grows
    // by the cap size to keep every wall pin on the straight side walls.
    const h = Math.max(58, 36 + rows * ROW_H) + (n.kind === "Interface" ? IFACE_CAP : 0);

    // Place pins explicitly (FIXED_POS) so we control their Y — ELK's BEGIN
    // alignment flushes the top pin to the box top edge. We shift the top pin
    // down into the second pin's slot (one row pitch) and move every pin with
    // it, clamping the shift so the bottom pin still fits inside the box.
    const pitch = ROW_H;
    const base = PIN_HALF; // top pin's py with its triangle top flush to the wall
    const bottomY = base + (rows - 1) * pitch; // py of the lowest pin before shift
    const shift = Math.max(0, Math.min(pitch, h - PIN_HALF - bottomY));
    const sidePort = (p: SchPort, i: number, x: number): ElkPort => ({
      id: portId(p.id),
      width: 0,
      height: 0,
      x,
      y: base + shift + i * pitch,
      layoutOptions: { "elk.port.side": p.side === "east" ? "EAST" : "WEST" },
    });
    return {
      id: nodeId(n.id),
      width: w,
      height: h,
      labels: [{ text: n.label }],
      layoutOptions: { "elk.portConstraints": "FIXED_POS" },
      ports: [
        ...west.map((p, i) => sidePort(p, i, 0)),
        ...east.map((p, i) => sidePort(p, i, w)),
      ],
    };
  }

  // An edge endpoint is a port if some box exposes it, else the box itself.
  const endpoint = (id: number) => (portOwner.has(id) ? portId(id) : nodeId(id));
  // #117: collapse each bundle trunk group into one box->port edge so ELK
  // routes (and reserves space for) a single wire; the renderer re-fans the
  // members at the consumer wall via `gatherBar`, keeping per-member
  // cross-probing on the stubs. Only layout sees the collapse.
  const trunks = new Map<number, TrunkGroup>();
  const droppedMembers = new Set<number>();
  for (const g of trunkGroups(graph)) {
    trunks.set(g.edges[0].id, g);
    for (const m of g.edges.slice(1)) droppedMembers.add(m.id);
  }
  const edges: ElkEdge[] = graph.edges
    .filter((e) => !droppedMembers.has(e.id))
    .map((e) => {
      const g = trunks.get(e.id);
      if (g)
        return {
          id: `e${e.id}`,
          // The representative is itself a member-pin <-> bundle-port edge, so
          // keeping its endpoints anchors the trunk at a real pin (ELK routes
          // it into the consumer wall at pin height, not off a box corner)
          // while preserving the model's signal direction for layering.
          sources: [endpoint(e.source)],
          targets: [endpoint(e.target)],
          // The trunk is the whole bundle, so it carries the bundle's name.
          labels: [{ text: g.name, width: g.name.length * 5.5, height: 11 }],
        };
      return {
        id: `e${e.id}`,
        sources: [endpoint(e.source)],
        targets: [endpoint(e.target)],
        // Give ELK the net label so it reserves space and returns a placement.
        labels: e.net ? [{ text: e.net, width: e.net.length * 5.5, height: 11 }] : undefined,
      };
    });

  return {
    id: "root",
    layoutOptions: {
      "elk.algorithm": "layered",
      "elk.direction": "RIGHT",
      // Compact spacing so blocks and pins stay close together (less hunting).
      "elk.layered.spacing.nodeNodeBetweenLayers": "55",
      "elk.spacing.nodeNode": "18",
      "elk.spacing.edgeNode": "12",
      "elk.spacing.edgeEdge": "8",
      "elk.layered.spacing.edgeNodeBetweenLayers": "12",
    },
    children,
    edges,
  };
}

// --- #117: bundle trunk wires ------------------------------------------------
/// A bundle (raw access) port's member-tap edges into one consumer box. The
/// per-member edges stay the model truth for cross-probing; layout and
/// rendering collapse them into a single trunk that fans out at the consumer.
export interface TrunkGroup {
  port: number; // the bundle port id
  box: number; // the consumer box node id
  side: SchPort["side"]; // the consumer wall the member pins sit on
  /// Trunk label: the bundle *instance's* label (e.g. `bus`), not its interface
  /// type — type names repeat across instances, and the renderer merges wire
  /// labels by text, so a type-named label would collapse two different
  /// bundles' trunks onto one (wrongly-targeted) label.
  name: string;
  path: string; // bundle port model path (the interface instance)
  edges: SchEdge[]; // >= 2 member taps, in graph order
}

/// Group a graph's edges into bundle trunks (#117): edges with a bundle pin on
/// one end and a plain pin of one *other* box on the other, keyed per (bundle
/// port, consumer box, wall) — a member pin can sit on either wall of the
/// consumer (picorv32 drives east, reads west), and a single-sided gather bar
/// cannot serve the far wall, so each wall gets its own trunk. Groups of one
/// stay ordinary wires. Edges anchored on a bare box (no pin) are left
/// ungrouped too — a fan-out needs member pins.
export function trunkGroups(graph: SchematicGraph): TrunkGroup[] {
  const bundle = new Map<number, SchPort>();
  const owner = new Map<number, number>(); // port id -> owning box id
  const side = new Map<number, SchPort["side"]>(); // port id -> its wall
  const label = new Map<number, string>(); // box id -> its label
  for (const n of graph.nodes) {
    label.set(n.id, n.label);
    for (const p of n.ports) {
      owner.set(p.id, n.id);
      side.set(p.id, p.side);
      if (p.bundle) bundle.set(p.id, p);
    }
  }
  const groups = new Map<string, TrunkGroup>();
  for (const e of graph.edges) {
    const bp = bundle.has(e.source) ? e.source : bundle.has(e.target) ? e.target : undefined;
    if (bp === undefined) continue;
    const other = bp === e.source ? e.target : e.source;
    // A bundle-to-bundle link (#106 modport view -> consumer pin) is already a
    // single wire — never trunk material.
    if (bundle.has(other)) continue;
    const box = owner.get(other);
    if (box === undefined || box === owner.get(bp)) continue;
    const wall = side.get(other)!;
    const key = `${bp}:${box}:${wall}`;
    let g = groups.get(key);
    if (!g) {
      const p = bundle.get(bp)!;
      const bx = owner.get(bp)!;
      g = { port: bp, box, side: wall, name: label.get(bx) ?? p.name, path: p.path ?? "", edges: [] };
      groups.set(key, g);
    }
    g.edges.push(e);
  }
  return [...groups.values()].filter((g) => g.edges.length > 1);
}

/// One trunk stub's length: how far the gather bar sits off the consumer wall.
/// Deliberately *inside* ELK's `elk.spacing.edgeNode` channel (12px) so the bar
/// never lies along other edges' vertical runs next to the box.
const TRUNK_STUB = 8;

/// Consumer-side fan-out geometry for a trunk (#117): a vertical gather bar one
/// stub-length off the pins' wall (`dir` = +1 off an east wall, -1 west) and a
/// horizontal stub per member pin. No separate joint: the trunk edge is
/// anchored at one of these pins, so its final approach crosses the bar inside
/// the bar's y-span.
export function gatherBar(pins: Pt[], dir: 1 | -1): { bar: [Pt, Pt]; stubs: [Pt, Pt][] } {
  const x = pins[0].x + TRUNK_STUB * dir;
  const ys = pins.map((p) => p.y);
  return {
    bar: [
      { x, y: Math.min(...ys) },
      { x, y: Math.max(...ys) },
    ],
    stubs: pins.map((p) => [p, { x, y: p.y }]),
  };
}

const elk = new ELK();

/// Lay out a SchematicGraph; returns the ELK graph with x/y/edge sections.
export async function layout(graph: SchematicGraph, opts: LayoutOpts = {}): Promise<any> {
  return elk.layout(toElk(graph, opts) as any);
}

/// Zoom factor that fits a laid-out graph (`baseW`x`baseH`) inside a pane
/// (`paneW`x`paneH`), clamped to <= `maxZoom`. The default cap of 1 keeps small
/// scopes at natural size on drill-in; an explicit fit (#114) passes a higher
/// cap so the schematic fills a pane larger than the graph. Returns 1 when the
/// pane is unmeasurable (e.g. a hidden or not-yet-laid-out host reports 0) to
/// avoid div-by-zero.
export function fitZoom(
  baseW: number,
  baseH: number,
  paneW: number,
  paneH: number,
  maxZoom = 1,
): number {
  if (baseW <= 0 || baseH <= 0 || paneW <= 0 || paneH <= 0) return 1;
  return Math.min(maxZoom, paneW / baseW, paneH / baseH);
}

// --- wire-label placement --------------------------------------------------
export interface Pt {
  x: number;
  y: number;
}
/// A viewport rectangle in base (pre-zoom) coordinates.
export interface VRect {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}
export interface LabelPlacement {
  x: number;
  y: number;
  /// Degrees to rotate the label about (x, y); 0 for a horizontal segment.
  rotate: number;
  anchor: string;
  baseline: string;
}

/// Where a net label sits on a wire segment `a`–`b`: centred on the segment,
/// nudged just above a horizontal wire, and rotated 90° to read *along* a
/// vertical wire instead of across it (#27).
export function wireLabelPlacement(a: Pt, b: Pt): LabelPlacement {
  const horizontal = Math.abs(a.x - b.x) >= Math.abs(a.y - b.y);
  const x = (a.x + b.x) / 2;
  const y = (a.y + b.y) / 2;
  return horizontal
    ? { x, y: y - 3, rotate: 0, anchor: "middle", baseline: "auto" }
    : { x, y, rotate: 90, anchor: "middle", baseline: "middle" };
}

/// Clip segment `a`–`b` to rectangle `r` (Liang–Barsky); returns the visible
/// sub-segment, or `null` if the segment lies wholly outside. Used to keep a
/// wire's label anchored to the part of the wire currently on screen (#28).
export function clampSegmentToRect(a: Pt, b: Pt, r: VRect): [Pt, Pt] | null {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const p = [-dx, dx, -dy, dy];
  const q = [a.x - r.x0, r.x1 - a.x, a.y - r.y0, r.y1 - a.y];
  let t0 = 0;
  let t1 = 1;
  for (let i = 0; i < 4; i++) {
    if (p[i] === 0) {
      if (q[i] < 0) return null; // parallel to this edge and outside it
    } else {
      const t = q[i] / p[i];
      if (p[i] < 0) {
        if (t > t1) return null;
        if (t > t0) t0 = t;
      } else {
        if (t < t0) return null;
        if (t < t1) t1 = t;
      }
    }
  }
  return [
    { x: a.x + t0 * dx, y: a.y + t0 * dy },
    { x: a.x + t1 * dx, y: a.y + t1 * dy },
  ];
}

/// Manhattan length of a segment. The metric `placeWireLabels` has always used
/// to pick "longest"; kept in one place so the live pick and the precomputed
/// off-screen fallback cannot disagree about which segment wins (#263).
function manhattan(a: Pt, b: Pt): number {
  return Math.abs(a.x - b.x) + Math.abs(a.y - b.y);
}

/// Bounding box of every segment of a net; `null` for an empty set.
export function segmentsAabb(segs: [Pt, Pt][]): VRect | null {
  if (!segs.length) return null;
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const [a, b] of segs) {
    x0 = Math.min(x0, a.x, b.x);
    y0 = Math.min(y0, a.y, b.y);
    x1 = Math.max(x1, a.x, b.x);
    y1 = Math.max(y1, a.y, b.y);
  }
  return { x0, y0, x1, y1 };
}

/// The longest segment overall by Manhattan length — where a label parks when
/// its whole net is off-screen. Ties keep the first, matching the original loop.
export function longestSegment(segs: [Pt, Pt][]): [Pt, Pt] | null {
  let best: [Pt, Pt] | null = null;
  let bestLen = -1;
  for (const seg of segs) {
    const len = manhattan(seg[0], seg[1]);
    if (len > bestLen) [bestLen, best] = [len, seg];
  }
  return best;
}

/// Do two rectangles overlap? **Inclusive** on the boundary, because
/// `clampSegmentToRect` keeps a segment lying exactly on the viewport edge —
/// an exclusive test here would reject a label the full clip would have placed.
export function rectsIntersect(a: VRect, b: VRect): boolean {
  return a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1;
}

/// A label's pan-invariant geometry, computed once per render (#263). Both
/// fields depend only on the laid-out wire, so recomputing them per scroll
/// event — as `placeWireLabels` used to — is pure waste.
export interface LabelGeom {
  /// Bounding box of every segment, for the cheap off-screen reject.
  aabb: VRect | null;
  /// The segment an off-screen label parks on.
  fallback: [Pt, Pt] | null;
}

export function labelGeometry(segs: [Pt, Pt][]): LabelGeom {
  return { aabb: segmentsAabb(segs), fallback: longestSegment(segs) };
}

/// Where a net's label belongs for the current viewport: centred on the longest
/// *visible* part of the wire, or parked on `geom.fallback` when the net is
/// wholly off-screen. Equivalent to the two-pass loop this replaced (#263) —
/// the AABB test only skips a per-segment clip that could not have matched.
export function chooseLabelSegment(
  segs: [Pt, Pt][],
  view: VRect,
  geom: LabelGeom,
): LabelPlacement | null {
  let best: [Pt, Pt] | null = null;
  if (geom.aabb && rectsIntersect(geom.aabb, view)) {
    let bestLen = -1;
    for (const [a, b] of segs) {
      const vis = clampSegmentToRect(a, b, view);
      if (!vis) continue;
      const len = manhattan(vis[0], vis[1]);
      if (len > bestLen) [bestLen, best] = [len, vis];
    }
  }
  best ??= geom.fallback;
  return best ? wireLabelPlacement(best[0], best[1]) : null;
}

/// Has a label's placement actually moved? Guards the 4–6 `setAttribute` calls
/// per label: writing an unchanged value still dirties layout, which is what
/// made the next pan's viewport read force a re-layout (#263).
export function placementsEqual(a: LabelPlacement | null, b: LabelPlacement): boolean {
  return (
    a !== null &&
    a.x === b.x &&
    a.y === b.y &&
    a.rotate === b.rotate &&
    a.anchor === b.anchor &&
    a.baseline === b.baseline
  );
}
