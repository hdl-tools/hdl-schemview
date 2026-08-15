// DTOs mirroring svxprobe-gui / svxprobe-schematic serialized shapes.

export type Side = "west" | "east";

/**
 * Schematic granularity (#157) mirroring the Rust `Projection` enum (kebab-case
 * serde). `"process-level"` is the default one-box-per-process view; `"gate-level"`
 * dissolves each drilled combinational block into its gate/mux primitives.
 */
export type Projection = "process-level" | "gate-level";

/**
 * Which way to follow connectivity, mirroring the Rust `Dir` enum (lowercase
 * serde). Note `Edge.dir` in the model is relative to the edge's port end; on the
 * trace API this is the user-facing sense — `"in"` is fan-in (what drives this),
 * `"out"` is fan-out (what this reaches).
 */
export type Dir = "in" | "out" | "inout";

/**
 * One expansion step of a schematic trace (#244 PR2).
 *
 * The seed is a canonical model **path** — what a pin, a wire and a box already
 * carry — rather than a node id, so a step survives in a detached pane's
 * localStorage snapshot across a reload. `depth` defaults to one hop, because the
 * affordance behind it is a single click on a fan-in/fan-out control.
 */
export interface TraceStep {
  path: string;
  dir: Dir;
  depth?: number;
  /**
   * Overrides the shared `ConeLimits.fanout` for this step only (#244 PR4) — what
   * the "N more…" badge sends, so expanding one capped signal reveals its
   * remainder without un-capping every other signal in the trace. Not clamped to
   * the shared budget (exceeding it is the point); `ConeLimits.boxes` still bounds
   * the result, so this degrades gracefully rather than blowing up on a hot net.
   */
  fanout?: number;
}

/**
 * The trace traversal budget (#244), mirroring the Rust `ConeLimits`. Every field
 * is optional and inherits its default independently, so a caller that cares only
 * about fan-out passes `{ fanout: 8 }`. What a cap drops is always reported —
 * `SchPort.more` and `SchematicGraph.truncated` — never silently discarded.
 */
export interface ConeLimits {
  depth?: number;
  fanout?: number;
  boxes?: number;
}

export interface SchPort {
  id: number;
  name: string;
  side: Side;
  /** Canonical model path of the signal this pin represents; right-click cross-probes it. */
  path?: string;
  /** Bit-range like "[31:0]" for a bus pin, absent for a scalar. */
  width?: string;
  /**
   * Structural role of a synthesized FF/latch/memory/gate pin — a model fact
   * from the harness (clock name / async-reset path / latch gating path /
   * memory port), absent for plain data pins and all module-instance ports.
   * `"sel"` (#157) marks a gate-level `Mux`'s select input, placed on the
   * trapezoid's south wall to distinguish it from the west data branches.
   */
  role?:
    | "clk"
    | "reset"
    | "enable"
    | "addr"
    | "din"
    | "dout"
    | "write"
    | "read"
    | "sel"
    | "inv";
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
  /**
   * Literal/parameter value tied to this input (#199): a gate/mux/datapath operand
   * that is a hard-coded constant or a parameter. Drawn as an inline tie value
   * beside the pin (so it is traceable at the gate); absent for net-driven pins.
   */
  constant?: string;
  /**
   * Connections on this pin a cone's fan-out cap dropped (#244) — the count behind
   * a "N more…" affordance. Absent on every scope-graph pin and on any cone pin
   * whose whole fan-out fit; a capped signal always reports its remainder here
   * rather than dropping it silently.
   */
  more?: number;
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
   * `initSource`. Under the gate-level projection (#157) a combinational block
   * dissolves into gate primitives — `Mux` → trapezoid (select pin on the south
   * wall, `role: "sel"`); `And`/`Or`/`Xor`/`Nand`/`Nor`/`Xnor`/`Not`/`Buf` →
   * IEEE distinctive shapes; `Add`/`Sub`/`Mul`/`Cmp`/`Shift` → labelled datapath
   * boxes (the operator rides in `label`).
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
  /**
   * The instance that contains this box, when the graph spans more than one
   * scope (#293).
   *
   * Trace mode is the only view that puts objects from several scopes on one
   * canvas — every other view *is* a single scope, so the frame carries the
   * answer. Once the walk crosses a wall, "which module is this in?" becomes a
   * question the drawing has to answer.
   *
   * The container is the nearest ancestor `Instance`; generate blocks dissolve,
   * exactly as the backend's `child_boxes` dissolves them. Absent for a box
   * directly under the design top, so a trace is never wrapped in one useless
   * outer box — and always absent from the hierarchy view, which shows one
   * scope and so nests nothing.
   */
  parent?: number;
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
  /**
   * Literal the net is unconditionally tied to (e.g. "1'b0" for `assign
   * pcpi_mul_wr = 0;`), so the wire label can read `pcpi_mul_wr = 1'b0` (#298).
   * Set by the backend only when the net's sole driver is a logic block the
   * model marks constant; absent for a computed net, a bit-selected wire, and
   * every synthetic wire. Never re-derived here — the frontend does no folding.
   */
  constant?: string;
}
export interface SchematicGraph {
  root: string;
  nodes: SchNode[];
  edges: SchEdge[];
  /**
   * Set when a cone traversal cap engaged (#244), so a view can show a banner even
   * when the truncated pin is scrolled off canvas. Absent for scope graphs.
   */
  truncated?: boolean;
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
  /** Last line the construct spans (1-based, inclusive); highlight is by line (#203). */
  end_line: number;
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
  /**
   * The cross-language counterpart location (#159): the C/C++ span that `source`'s
   * generated-RTL span maps to via the HLS provenance map, when one exists. Lets the
   * frontend highlight both the RTL and C panes from one probe. Absent otherwise.
   */
  mapped_source?: SourceLoc | null;
  wave: WaveLink;
  alternatives: NodeRef[];
}

/**
 * One source file in the loaded design (#159). Mirrors svxprobe-gui's `SourceFile`.
 * `language` is `"systemverilog"` for RTL, `"c"`/`"cpp"` for HLS C/C++ sources
 * (absent ⇒ SystemVerilog). The frontend reveals a C/C++ pane when a non-SV file exists.
 */
export interface SourceFile {
  id: number;
  path: string;
  language?: string | null;
}
/**
 * One identifier occurrence in a source file (#225) — a declaration name token or a
 * resolved value reference — for the source pane's semantic coloring. Mirrors
 * svxprobe-gui's `NameRefDto`. `line`/`col` are 1-based; `cls` is the kebab-case
 * `NameClass` (`signal`, `port`, `param`, `enum-member`, …), rendered as `.tok-name-<cls>`.
 */
export interface NameRefDto {
  line: number;
  col: number;
  len: number;
  cls: string;
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
