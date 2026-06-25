// Adapter from our SchematicGraph to an ELK graph, plus a layout helper.
// `toElk` is pure (unit-tested); `layout` runs elkjs.
import ELK from "elkjs/lib/elk.bundled.js";
import type { SchematicGraph, SchPort } from "./types";

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

const pinLabelLen = (p: SchPort) => p.name.length + (p.width ? p.width.length + 1 : 0);

/// Pure mapping: SchematicGraph -> ELK graph (no geometry yet).
export function toElk(graph: SchematicGraph): ElkGraph {
  const portOwner = new Set<number>();
  for (const n of graph.nodes) for (const p of n.ports) portOwner.add(p.id);

  const children: ElkChild[] = graph.nodes.map((n): ElkChild => {
    // Boundary I/O pin: a small node sized to its label, with its single port
    // already sided toward the design.
    if (n.kind === "Port") {
      const lab = n.ports[0] ? pinLabelLen(n.ports[0]) : n.label.length;
      // Cluster all input pins in a dedicated first column and outputs in a
      // dedicated last column, so I/O is grouped and easy to find.
      const input = n.ports[0]?.side === "east";
      return {
        id: nodeId(n.id),
        width: Math.max(40, lab * PIN_CH + 24),
        height: 26,
        labels: [{ text: n.label }],
        layoutOptions: {
          "elk.portConstraints": "FIXED_SIDE",
          "elk.layered.layering.layerConstraint": input ? "FIRST_SEPARATE" : "LAST_SEPARATE",
        },
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
    // Constant-tied inputs show their literal inside the box, before the name.
    const westLen = (p: SchPort) => pinLabelLen(p) + (p.constant ? p.constant.length + 2 : 0);
    const wMax = west.reduce((m, p) => Math.max(m, westLen(p)), 0);
    const eMax = east.reduce((m, p) => Math.max(m, pinLabelLen(p)), 0);
    const titleLen = Math.max(n.label.length, n.module ? n.module.length + 2 : 0);
    // Wide enough for the title and for the west+east pin labels side by side.
    const w = Math.max(150, titleLen * TITLE_CH + 28, (wMax + eMax) * PIN_CH + 56);
    const rows = Math.max(west.length, east.length, 1);
    // Tall enough for the two-line title band plus one row per pin.
    const h = Math.max(58, 36 + rows * ROW_H);
    return {
      id: nodeId(n.id),
      width: w,
      height: h,
      labels: [{ text: n.label }],
      layoutOptions: {
        "elk.portConstraints": "FIXED_SIDE",
        "elk.spacing.portPort": "10",
        // Pack pins at the top of each side so they cluster (no justify-spread
        // when one side has fewer pins than the other).
        "elk.portAlignment.west": "BEGIN",
        "elk.portAlignment.east": "BEGIN",
      },
      ports: n.ports.map((p) => ({
        id: portId(p.id),
        width: 8,
        height: 8,
        layoutOptions: { "elk.port.side": p.side === "east" ? "EAST" : "WEST" },
      })),
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
