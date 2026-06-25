// Adapter from our SchematicGraph to an ELK graph, plus a layout helper.
// `toElk` is pure (unit-tested); `layout` runs elkjs.
import ELK from "elkjs/lib/elk.bundled.js";
import type { SchematicGraph } from "./types";

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
  labels: { text: string }[];
  ports: ElkPort[];
  layoutOptions: Record<string, string>;
  x?: number;
  y?: number;
}
export interface ElkEdge {
  id: string;
  sources: string[];
  targets: string[];
}
export interface ElkGraph {
  id: string;
  layoutOptions: Record<string, string>;
  children: ElkChild[];
  edges: ElkEdge[];
}

export const nodeId = (id: number) => `n${id}`;
export const portId = (id: number) => `p${id}`;

/// Pure mapping: SchematicGraph -> ELK graph (no geometry yet).
export function toElk(graph: SchematicGraph): ElkGraph {
  const portOwner = new Set<number>();
  for (const n of graph.nodes) for (const p of n.ports) portOwner.add(p.id);

  const children: ElkChild[] = graph.nodes.map((n) => {
    const h = Math.max(46, n.ports.length * 16 + 24);
    const w = Math.max(120, n.label.length * 9 + 48);
    return {
      id: nodeId(n.id),
      width: w,
      height: h,
      labels: [{ text: n.label }],
      layoutOptions: { "elk.portConstraints": "FIXED_SIDE" },
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
  }));

  return {
    id: "root",
    layoutOptions: {
      "elk.algorithm": "layered",
      "elk.direction": "RIGHT",
      "elk.layered.spacing.nodeNodeBetweenLayers": "60",
      "elk.spacing.nodeNode": "30",
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
