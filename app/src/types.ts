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
  role?: "clk" | "reset" | "enable" | "addr" | "din" | "dout" | "write" | "read";
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
   * `Latch` → tinted storage box with an "LE" caption, `Interface` → hexagon
   * "bundle" box for an instance, or a square frame pin when `modport` is set
   * (#125). `Memory` → a MEMORY array glyph (#112) with addr/din/dout/read/write
   * pins (`SchPort.role`), labelled with `memDepth` and an INIT tab from
   * `initSource`.
   */
  kind: string;
  label: string;
  path: string;
  expandable: boolean;
  ports: SchPort[];
  /** Module/definition type of an instance (e.g. "picorv32"). */
  module?: string;
  /** Memory only (#112): word count of the array (e.g. 512 for `ram [0:511]`). */
  memDepth?: number;
  /** Memory only (#112): `$readmemh` source-file arg text; presence shows the INIT tab. */
  initSource?: string;
  /** Literal of a constant-source node (e.g. "32'd0"); drives one tied input. */
  constant?: string;
  /**
   * Modport view of a modport-qualified interface port (e.g. "mem"); absent
   * for bare interface instances. Draws as a square frame pin in the boundary
   * frame column (#125), sublabelled `(mem_if.mem)`, with every wired member
   * wire anchored at the square (unconnected members omitted).
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
/**
 * One signal declared directly inside a scope (#171) — a row of a waveform pane's
 * signal picker. Mirrors svxprobe-gui's `SignalEntry`. The tree lists the scopes
 * (`hierarchyTree`); this lists what is inside one.
 */
export interface SignalEntry {
  /** Canonical model path — the picker's key; feeds probeNode → addToWaveform. */
  path: string;
  name: string;
  /**
   * The cross-probe's representative node kind for this path. Note the backing
   * `Var` outranks its `Port` in the port/backing-net dual node, so a module port
   * reports "Var" — it names the object a click actually selects.
   */
  kind: "Port" | "Net" | "Var" | "Memory";
  /** Bit-range like "[31:0]" (enum-width fallback included); absent for a scalar. */
  width?: string;
  /** Whether this pane's trace carries it; false → the row shows dimmed, not pruned. */
  in_trace: boolean;
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

// CLI launch args parsed by the Tauri shell before the window opened (#136),
// mirroring svxprobe-gui's StartupArgs. Present only when the app was launched
// with `-f <filelist> -top <name> [-I <dir>]... [-trace <path>] [-src-root <dir>]`;
// the frontend prefills the load form from it and auto-loads.
export interface StartupArgs {
  filelist: string;
  top: string;
  incdirs: string[];
  trace: string;
  src_root: string;
}
