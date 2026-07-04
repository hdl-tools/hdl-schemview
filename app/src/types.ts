// DTOs mirroring svxprobe-gui / svxprobe-schematic serialized shapes.

export type Side = "west" | "east";

export interface SchPort {
  id: number;
  name: string;
  side: Side;
  /** Canonical model path of the signal this pin represents; right-click cross-probes it. */
  path?: string;
  /** Bit-range like "[31:0]" for a bus pin, absent for a scalar. */
  width?: string;
  /**
   * Structural role of a synthesized FF/latch pin (#59) — a model fact from
   * the harness (clock name / async-reset path / latch gating path), absent
   * for plain data pins and all module-instance ports.
   */
  role?: "clk" | "reset" | "enable";
  /**
   * Marks a bundle pin — a whole-interface connection (#106 consumer bundle
   * pin, #96 aggregate access ports). Drawn square instead of the directional
   * triangle. Absent for normal pins.
   */
  bundle?: boolean;
  /**
   * Marks a pin nothing connects to (#118): an unconnected instance output /
   * floating input, or a logic-box output no in-scope box reads. Drawn dimmed;
   * absent for connected pins.
   */
  dangling?: boolean;
}
export interface SchNode {
  id: number;
  /**
   * Model NodeKind, mirrored as a string. Drives the renderer's glyph choice:
   * `Instance`/`GenBlock` → module box, `Port` → boundary pin, `FF` → flip-flop,
   * `Comb` → combinational rectangle, `Assign` → stadium (function) node,
   * `Latch` → tinted storage box with an "LE" caption, `Interface` →
   * folded-corner "bundle" box.
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
  /**
   * Modport view of a modport-qualified interface port (e.g. "mem"); absent
   * for bare interface instances. Marks the bundle as boundary-like: it
   * clusters at the frame and is sublabelled `(mem_if.mem)`.
   */
  modport?: string;
}
export interface SchEdge {
  id: number;
  source: number;
  target: number;
  /** Connecting net name, relative to the scope (e.g. "bus.valid"). */
  net?: string;
  /**
   * Canonical model path of the connecting net (e.g.
   * "picorv32_soc.g_lane[0].bus.valid") — absolute, no bit-select. Lets a wire
   * click cross-probe via `probe_node`. Absent for synthetic constant tie-offs.
   */
  net_path?: string;
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
  // value→name members when the signal is enum-typed (FSM state display, #81).
  enum_map?: { name: string; value: number }[];
}
export interface ProbeResponse {
  anchor: NodeRef;
  source: SourceLoc | null;
  wave: WaveLink;
  alternatives: NodeRef[];
}
/**
 * One node of the lazy instance-hierarchy tree (#92). `children` is populated
 * to the requested depth only; `expandable` flags nodes with more levels below,
 * fetched on demand via `hierarchy_tree(path, 1)`.
 */
export interface TreeNode {
  /** Last path segment (e.g. "g_lane[0]", "memory"). */
  label: string;
  /** Canonical model path — feeds setScope / scope_graph. */
  path: string;
  /** Module/interface type sublabel (e.g. "picorv32"). */
  module?: string;
  expandable: boolean;
  children: TreeNode[];
}
export interface ValueChange {
  time: number;
  value: string;
}

// Trace timescale: each raw ValueChange.time tick equals `factor` of `unit`
// (normalized short unit: "fs"/"ps"/"ns"/"us"/"ms"/"s", "" when unknown).
export interface TraceTimescale {
  factor: number;
  unit: string;
}
