// DTOs mirroring svxprobe-gui / svxprobe-schematic serialized shapes.

export type Side = "west" | "east";

export interface SchPort {
  id: number;
  name: string;
  side: Side;
  /** Bit-range like "[31:0]" for a bus pin, absent for a scalar. */
  width?: string;
}
export interface SchNode {
  id: number;
  /**
   * Model NodeKind, mirrored as a string. Drives the renderer's glyph choice:
   * `Instance`/`GenBlock` → module box, `Port` → boundary pin, `FF` → flip-flop,
   * `Comb` → combinational rectangle, `Assign` → stadium (function) node,
   * `Latch` → tinted storage box with an "LE" caption.
   */
  kind: string;
  label: string;
  path: string;
  expandable: boolean;
  ports: SchPort[];
  /** Module/definition type of an instance (e.g. "picorv32"). */
  module?: string;
  /** Literal of a constant-source node (e.g. "32'd0"); drives one tied input. */
  constant?: string;
}
export interface SchEdge {
  id: number;
  source: number;
  target: number;
  /** Connecting net name, relative to the scope (e.g. "bus.valid"). */
  net?: string;
}
export interface SchematicGraph {
  root: string;
  nodes: SchNode[];
  edges: SchEdge[];
}

export interface NodeRef {
  id: number;
  path: string;
  kind: string;
}
export interface SourceLoc {
  file: number;
  path: string;
  line: number;
  col: number;
  offset: number;
  end_offset: number;
}
export interface WaveLink {
  in_trace: boolean;
  var_ref: number;
  signal_ref: number;
  full_name: string;
}
export interface ProbeResponse {
  anchor: NodeRef;
  source: SourceLoc | null;
  wave: WaveLink;
  alternatives: NodeRef[];
}
export interface ValueChange {
  time: number;
  value: string;
}
