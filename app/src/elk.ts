// Adapter from our SchematicGraph to an ELK graph, plus a layout helper.
// `toElk` is pure (unit-tested); `layout` runs elkjs.
import ELK from "elkjs/lib/elk.bundled.js";
import type { SchematicGraph, SchNode, SchPort } from "./types";

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

// --- inferred FF symbol ----------------------------------------------------
export type FfRole = "clk" | "reset" | "q" | "data";
// Classify an FF pin by side + (conventional) name: Q on the east, clock/reset
// by name, everything else a condition.
export function ffRole(p: SchPort): FfRole {
  if (p.side === "east") return "q";
  if (/(^|_)(clk|clock)/i.test(p.name)) return "clk";
  if (/(^|_)(rst|reset)/i.test(p.name)) return "reset";
  return "data";
}
export const FF_H = 46;
export const ffWidth = (dataCount: number) => Math.max(56, (dataCount + 1) * 20 + 24);

// A fixed-size FF: clock flush bottom-left, reset next, conditions spread along
// the bottom, Q on the right centre (FIXED_POS so the renderer can match glyphs).
function ffChild(n: SchNode): ElkChild {
  const by = (r: FfRole) => n.ports.filter((p) => ffRole(p) === r);
  const data = by("data");
  const W = ffWidth(data.length);
  const south = (id: number, x: number): ElkPort => ({
    id: portId(id),
    width: 6,
    height: 6,
    x,
    y: FF_H,
    layoutOptions: { "elk.port.side": "SOUTH" },
  });
  const ports: ElkPort[] = [];
  for (const p of by("q"))
    ports.push({
      id: portId(p.id),
      width: 6,
      height: 6,
      x: W,
      y: FF_H / 2,
      layoutOptions: { "elk.port.side": "EAST" },
    });
  for (const p of by("clk")) ports.push(south(p.id, 11));
  for (const p of by("reset")) ports.push(south(p.id, 25));
  const x0 = 40;
  const x1 = W - 12;
  data.forEach((p, i) =>
    ports.push(south(p.id, data.length > 1 ? x0 + ((x1 - x0) * i) / (data.length - 1) : (x0 + x1) / 2)),
  );
  return {
    id: nodeId(n.id),
    width: W,
    height: FF_H,
    labels: [{ text: "FF" }],
    layoutOptions: { "elk.portConstraints": "FIXED_POS" },
    ports,
  };
}

/// Pure mapping: SchematicGraph -> ELK graph (no geometry yet).
export function toElk(graph: SchematicGraph): ElkGraph {
  const portOwner = new Set<number>();
  for (const n of graph.nodes) for (const p of n.ports) portOwner.add(p.id);

  const children: ElkChild[] = graph.nodes.map((n): ElkChild => {
    // Inferred register: a fixed-size FF symbol with explicitly-placed pins —
    // clock + reset + conditions along the bottom, Q on the right centre.
    if (n.kind === "FF") return ffChild(n);
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
          width: 6,
          height: 6,
          layoutOptions: { "elk.port.side": p.side === "east" ? "EAST" : "WEST" },
        })),
      };
    }
    const west = n.ports.filter((p) => p.side !== "east");
    const east = n.ports.filter((p) => p.side === "east");
    const wMax = west.reduce((m, p) => Math.max(m, pinLabelLen(p)), 0);
    const eMax = east.reduce((m, p) => Math.max(m, pinLabelLen(p)), 0);
    const titleLen = Math.max(n.label.length, n.module ? n.module.length + 2 : 0);
    // Wide enough for the title and for the west+east pin labels side by side.
    const w = Math.max(150, titleLen * TITLE_CH + 28, (wMax + eMax) * PIN_CH + 56);
    const rows = Math.max(west.length, east.length, 1);
    // Tall enough for the two-line title band plus one row per pin.
    const h = Math.max(58, 36 + rows * ROW_H);

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
      width: 8,
      height: 8,
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
  });

  // An edge endpoint is a port if some box exposes it, else the box itself.
  const endpoint = (id: number) => (portOwner.has(id) ? portId(id) : nodeId(id));
  const edges: ElkEdge[] = graph.edges.map((e) => ({
    id: `e${e.id}`,
    sources: [endpoint(e.source)],
    targets: [endpoint(e.target)],
    // Give ELK the net label so it reserves space and returns a placement.
    labels: e.net ? [{ text: e.net, width: e.net.length * 5.5, height: 11 }] : undefined,
  }));

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

const elk = new ELK();

/// Lay out a SchematicGraph; returns the ELK graph with x/y/edge sections.
export async function layout(graph: SchematicGraph): Promise<any> {
  return elk.layout(toElk(graph) as any);
}
