//! Schematic extraction (roadmap Phase 3 — the third projection).
//!
//! Turns the elaborated model into a layout-agnostic graph the GUI lays out with
//! ELK. **Identity is free**: every schematic node/port carries its model NodeId,
//! so clicking a box or wire feeds straight into the cross-probe Selection bus —
//! no schematic-specific id space.
//!
//! Three views are produced, all on demand (never the whole design at once):
//! * [`scope_graph`] — the boxes (child instances) + wiring inside one scope,
//! * [`expand`] — the same, one level down into an instance,
//! * [`cone`] — the fan-in/out of a net (its driving/loading boxes).

use serde::{Deserialize, Serialize};
use svxprobe_model::{Design, Dir, Edge, MemPort, MuxPort, Node, NodeId, NodeKind};

/// Which border a port sits on (drives ELK port placement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    West,
    East,
}

/// Structural role of a synthesized logic pin (#59). Derived from model facts
/// the harness emits — the FF's timing-control clock name (`Node.type_`), its
/// async-reset path (`Node.reset`), the latch's gating path (`Node.enable`) —
/// never from pin-name patterns. Drives the FF/latch glyph (clock wedge,
/// reset/enable markers) in the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PinRole {
    Clk,
    Reset,
    Enable,
    /// Address input of a `Memory` glyph (the array-index expression) (#112).
    Addr,
    /// Data input of a `Memory` glyph (the written value) (#112).
    Din,
    /// Data output of a `Memory` glyph (the read value) (#112).
    Dout,
    /// Write-enable of a `Memory` glyph — the process that stores into it (#112).
    Write,
    /// Read-enable of a `Memory` glyph — the process that reads from it (#112).
    Read,
    /// Select (control) input of a `Mux` glyph (#157, ADR 0005) — placed on the
    /// trapezoid's south wall by the frontend, distinguishing it from the west
    /// data-branch pins. Set from the `MuxPort::Sel` edge role, never guessed.
    Sel,
    /// An inverted gate input (#157): a `~signal` operand whose single-fanout `Not`
    /// primitive was folded into an inversion bubble on this pin instead of a
    /// separate inverter box. The pin's `path` is the *un-inverted* operand (the
    /// signal driving the folded `Not`), so cross-probe still lands on it.
    Inv,
}

/// Which projection [`scope_graph_with`]/[`expand_with`] render (#157, ADR 0005).
/// The default [`Projection::ProcessLevel`] is byte-identical to the bare
/// [`scope_graph`]/[`expand`] entry points — one box per `always`/`assign`.
/// [`Projection::GateLevel`] is the opt-in view: each combinational block dissolves
/// into its gate/mux primitive network. It never displaces the default — a UI
/// toggle selects it, and it only surfaces the primitives the harness's opt-in
/// `--gate-level` pass emits (a model without them renders identically either way).
/// Serialized (kebab-case) as `"process-level"` / `"gate-level"` — the wire form
/// the frontend passes to the `scope_graph`/`expand_node` Tauri commands (#157 PR4);
/// an omitted param deserializes nothing and the command falls back to the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Projection {
    /// One box per source construct — the ADR 0004 default.
    #[default]
    ProcessLevel,
    /// Combinational blocks dissolved into gate/mux primitives — the ADR 0005 opt-in.
    GateLevel,
}

/// A pin on a box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchPort {
    pub id: NodeId,
    pub name: String,
    pub side: Side,
    /// Canonical model path of the signal this pin represents (the port's own path,
    /// or for a synthesized logic pin the carried signal's path). Lets a right-click
    /// on a pin cross-probe to source via `probe_node`. Empty for synthetic pins
    /// with no model node (e.g. constant tie-offs).
    pub path: String,
    /// Bit-range of the pin (`[31:0]`) parsed from its declared type, or `None`
    /// for a scalar. Shown next to the pin label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<String>,
    /// Structural role of a synthesized FF/latch pin; `None` for plain data pins
    /// and all module-instance ports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<PinRole>,
    /// Marks a bundle pin — a whole-interface connection rather than one
    /// signal: a consumer's modport-qualified port (#106) and a bare bundle's
    /// aggregate access ports (#96). Drawn square, unlike the directional
    /// triangle of a normal scalar/bus pin.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bundle: bool,
    /// Marks a pin nothing connects to (#118): an instance port with no model
    /// edge and no constant tie-off, or a synthesized logic output no box in
    /// the scope reads. Shown dimmed so a floating pin reads as intentionally
    /// unconnected rather than a rendering bug.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dangling: bool,
    /// Literal/parameter value tied to this input (#199): a gate/mux/datapath
    /// operand that is a hard-coded constant or a parameter. The frontend draws it
    /// as a tie value inline beside the pin (so it is traceable at the gate),
    /// instead of a separate source node. `None` for a net-driven pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constant: Option<String>,
    /// Connections on this pin a cone's fan-out cap dropped (#244) — the count
    /// behind a "N more…" affordance. `None` on every scope-graph pin and on any
    /// cone pin whose whole fan-out fit. Truncation is a *rendering* policy, so
    /// what it drops is always reported: a capped signal records its remainder
    /// here and sets [`SchematicGraph::truncated`], never silently vanishing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub more: Option<u32>,
}

/// A box in the schematic (an instance), carrying its model identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub label: String,
    pub path: String,
    pub expandable: bool,
    pub ports: Vec<SchPort>,
    /// Module/definition type of an instance (e.g. `picorv32`), recovered from
    /// the basename of its defining source file. `None` for non-instances.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// Literal of a constant-source node (`32'd0`); `None` for normal nodes. Such
    /// a node sits outside the design and drives a single tied input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constant: Option<String>,
    /// Modport view of a modport-qualified interface port (`mem`); `None` for
    /// bare interface instances and every other node kind. Marks the bundle as
    /// boundary-like so the frontend clusters it at the frame (#106) and
    /// sublabels it with the view (`mem_if.mem`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modport: Option<String>,
    /// The instance that contains this box, when the graph spans more than one
    /// scope (#293).
    ///
    /// Trace mode is the only view that puts objects from several scopes on one
    /// canvas — every other view *is* a single scope, so the frame carries the
    /// answer. Once the walk crosses a wall, "which module is this in?" becomes
    /// a question the drawing has to answer, and flattening it away discards a
    /// fact the elaborated hierarchy already states.
    ///
    /// The container is the nearest ancestor `Instance`; generate blocks
    /// dissolve, exactly as [`child_boxes`] dissolves them. `None` for a box
    /// directly under the design top, so a trace is never wrapped in one
    /// useless outer box. Always `None` from [`scope_graph`], which keeps that
    /// output byte-identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<NodeId>,
    /// Word count of a `Memory` box (#112) — labels the array (e.g. `512`).
    /// `None` for every non-memory node.
    #[serde(rename = "memDepth", skip_serializing_if = "Option::is_none")]
    pub mem_depth: Option<u32>,
    /// `$readmemh` source-file text for a `Memory` box (#112) — presence drives
    /// the INIT marker. `None` for a memory with no initializer and every other
    /// node kind.
    #[serde(rename = "initSource", skip_serializing_if = "Option::is_none")]
    pub init_source: Option<String>,
}

/// Synthetic id base for constant-source nodes (keyed by the driven port id), so
/// they never collide with real model node/port ids.
const CONST_ID_BASE: NodeId = 1 << 28;

/// Synthetic id base for synthesized logic-box pins (FFs today, combinational
/// boxes next). Above `CONST_ID_BASE` and any real model NodeId; pins are handed
/// out per `(box, signal)` by [`PinAlloc`] so boxes sharing a net keep distinct
/// pins.
const LOGIC_PIN_BASE: NodeId = 1 << 30;

/// Synthetic id base for a bare interface bundle's raw-access port (#96), keyed
/// by the instance id — disjoint from `CONST_ID_BASE` (keyed by port id) and
/// below `LOGIC_PIN_BASE`.
const RAW_PORT_BASE: NodeId = 3 << 28;

/// Synthetic id base for a drilled interface's per-view boundary frame port
/// (#97), keyed by the `Modport` node id. Disjoint from `CONST_ID_BASE`
/// (`1 << 28`), `RAW_PORT_BASE` (`3 << 28`), and `LOGIC_PIN_BASE` (`1 << 30`).
const MODPORT_FRAME_BASE: NodeId = 2 << 28;

/// Allocates a distinct, stable synthetic pin id per `(box, signal)` pair. Keying
/// on the box (not the signal alone) keeps several boxes that share one net — a
/// clock fanning out to many registers — from collapsing onto a single pin.
struct PinAlloc {
    next: NodeId,
    map: std::collections::HashMap<(NodeId, NodeId), NodeId>,
}

impl PinAlloc {
    fn new() -> Self {
        Self {
            next: LOGIC_PIN_BASE,
            map: std::collections::HashMap::new(),
        }
    }

    /// The pin id for `(box, signal)`, allocating one on first use. Injective
    /// over the pair; allocation order follows the caller's deterministic edge
    /// traversal, so ids are stable across runs.
    fn pin(&mut self, bx: NodeId, sig: NodeId) -> NodeId {
        let next = &mut self.next;
        *self.map.entry((bx, sig)).or_insert_with(|| {
            let p = *next;
            *next += 1;
            p
        })
    }
}

/// A wire between two endpoints (a box or one of its ports), by model NodeId.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchEdge {
    pub id: u32,
    pub source: NodeId,
    pub target: NodeId,
    /// Name of the connecting net/signal relative to the scope (e.g. `bus.valid`
    /// or `core_trap`), used to label the wire. `None` if unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net: Option<String>,
    /// Canonical model path of the connecting net/signal node (e.g.
    /// `picorv32_soc.g_lane[0].bus.valid`) — no scope-relative trimming and no
    /// bit-select. Lets a wire click cross-probe to source/waveform as a pure
    /// `nodes_at_path` lookup. `None` for synthetic wires (constant tie-offs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net_path: Option<String>,
}

/// A renderable, layout-agnostic schematic graph for one scope or cone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchematicGraph {
    pub root: String,
    pub nodes: Vec<SchNode>,
    pub edges: Vec<SchEdge>,
    /// Set when a cone traversal cap engaged (#244), so a view can show a banner
    /// even when the specific truncated pin is scrolled off canvas. Always
    /// `false` for `scope_graph`/`expand` and for the legacy uncapped `cone`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

fn side_of(dir: Dir) -> Side {
    match dir {
        Dir::Out => Side::East,
        _ => Side::West,
    }
}

/// Mirrored pin side for a boundary-like node: the pin faces the design, so an
/// input enters from the west frame (pin on the east) and an output leaves at
/// the east frame (pin on the west). Used by the scope's own boundary pins and
/// by modport member pins — a modport-qualified interface port is the
/// consumer's bundled window to the outside, so its `in` members exit east
/// toward their readers and its `out` members are entered from the west.
fn boundary_side(dir: Dir) -> Side {
    match dir {
        Dir::Out => Side::West,
        _ => Side::East,
    }
}

/// The boxes shown inside a scope: child `Instance`s, with `GenBlock`s dissolved
/// — their leaf instances are pulled up so e.g. both `g_lane[*].core` sit
/// together at the top instead of behind a generate-array group box.
fn child_boxes(design: &Design, scope: NodeId, projection: Projection) -> Vec<NodeId> {
    let mut out = Vec::new();
    if let Some(n) = design.node(scope) {
        for &c in &n.children {
            match design.node(c).map(|x| x.kind) {
                Some(NodeKind::Instance)
                | Some(NodeKind::Interface)
                | Some(NodeKind::Ff)
                | Some(NodeKind::Memory) => out.push(c),
                // A combinational block (comb / assign / latch) is one box at the
                // process level; under `GateLevel` it dissolves into its gate/mux
                // primitives — its flat children (#157) — the way a `GenBlock`
                // dissolves into its instances. A block that carries no primitives
                // (a bare `assign y = x` with no operator) keeps its box, so the
                // direct connection still renders. Sequential (`Ff`) and `Memory`
                // blocks stay opaque at both levels — decomposing their input cloud
                // is a deferrable later slice (ADR 0005).
                Some(NodeKind::Comb) | Some(NodeKind::Latch) | Some(NodeKind::Assign) => {
                    let gates = gate_children(design, c);
                    if projection == Projection::GateLevel && !gates.is_empty() {
                        out.extend(gates);
                    } else {
                        out.push(c);
                    }
                }
                Some(NodeKind::GenBlock) => out.extend(child_boxes(design, c, projection)),
                _ => {}
            }
        }
    }
    out
}

/// The gate-level primitives (#157) that are flat children of a logic block — the
/// boxes a `GateLevel` projection surfaces when it dissolves the block.
fn gate_children(design: &Design, block: NodeId) -> Vec<NodeId> {
    design
        .node(block)
        .map(|n| {
            n.children
                .iter()
                .copied()
                // A single-fanout `~operand` inverter is folded into a bubble on its
                // consumer's pin (#157), so it is not drawn as its own box.
                .filter(|&c| is_gate(design, c) && !is_foldable_not(design, c))
                .collect()
        })
        .unwrap_or_default()
}

/// A bare interface *instance* (a signal bundle) that carries `modport` views —
/// the thing #97 drills into. Excludes a modport-qualified interface *port* on a
/// consumer (`modport` set) and a bundle with no views (nothing to drill to).
/// Public so the GUI's hierarchy tree agrees with the schematic on what is a
/// drillable interface scope (single source of truth, like [`module_of`]).
pub fn is_bare_interface(design: &Design, id: NodeId) -> bool {
    design.node(id).is_some_and(|n| {
        n.kind == NodeKind::Interface
            && n.modport.is_none()
            && n.children
                .iter()
                .any(|&c| is_kind(design, c, NodeKind::Modport))
    })
}

/// Whether `id` is a navigable scope root — the *single* predicate `scope_graph`
/// resolves against and the GUI's hierarchy tree lists (#184), so the schematic and
/// the tree can never disagree on what is navigable (both call this one function).
/// An `Instance` always is; a `GenBlock` only if [`genblk_is_navigable`]; any other
/// node only if it is a drillable bare interface bundle ([`is_bare_interface`]).
/// Public so the GUI reuses it, mirroring [`is_bare_interface`]/[`module_of`] as the
/// single source of truth.
pub fn is_navigable_scope(design: &Design, id: NodeId) -> bool {
    match design.node(id).map(|n| n.kind) {
        Some(NodeKind::Instance) => true,
        Some(NodeKind::GenBlock) => genblk_is_navigable(design, id),
        _ => is_bare_interface(design, id),
    }
}

/// Whether a `GenBlock` is a navigable design scope (#184). A generate block is a
/// place in the design only if its subtree holds a real design object — an `Instance`
/// or a bare `Interface`. A block that contains only logic (`comb`/`ff`/`assign`/nets,
/// or a `Memory` array — all of which dissolve into the parent via [`child_boxes`]) is
/// a syntactic wrapper: its contents already render in the enclosing module, so a
/// standalone scope for it would be redundant and the tree/schematic must not offer
/// one. The test is contents, not the generate keyword — `g_lane[0]` is a `GenBlock`
/// too, but holds instances, so it stays. Non-`GenBlock` ids return `false`.
fn genblk_is_navigable(design: &Design, id: NodeId) -> bool {
    is_kind(design, id, NodeKind::GenBlock) && genblk_has_design_content(design, id)
}

/// Recursive helper for [`genblk_is_navigable`]: does this node's subtree carry an
/// `Instance` or bare `Interface`, descending through nested generate blocks?
fn genblk_has_design_content(design: &Design, id: NodeId) -> bool {
    design.node(id).is_some_and(|n| {
        n.children
            .iter()
            .any(|&c| match design.node(c).map(|k| k.kind) {
                Some(NodeKind::Instance) => true,
                Some(NodeKind::GenBlock) => genblk_has_design_content(design, c),
                _ => is_bare_interface(design, c),
            })
    })
}

/// The box (from `boxes`) that `node` lives in — its nearest ancestor that is a
/// box. `None` if `node` is outside all of them.
fn box_of(
    design: &Design,
    node: NodeId,
    boxes: &std::collections::HashSet<NodeId>,
) -> Option<NodeId> {
    let mut cur = node;
    loop {
        if boxes.contains(&cur) {
            return Some(cur);
        }
        cur = design.node(cur)?.parent?;
    }
}

fn is_kind(design: &Design, id: NodeId, kind: NodeKind) -> bool {
    design.node(id).map(|n| n.kind) == Some(kind)
}

/// A process-level logic node (inferred register / combinational process /
/// continuous assign) or a `Memory` array (#112) — the boxes the signal-join pass
/// wires through shared nets (not the structural instance-port pass), and whose
/// unread outputs are kept as dangling pins. A memory's synthesized addr/din/dout
/// pins fold in exactly like an FF's, so it belongs to this set.
fn is_logic_box(design: &Design, id: NodeId) -> bool {
    matches!(
        design.node(id).map(|n| n.kind),
        Some(NodeKind::Ff)
            | Some(NodeKind::Comb)
            | Some(NodeKind::Latch)
            | Some(NodeKind::Assign)
            | Some(NodeKind::Memory)
    )
}

/// A gate-level primitive kind (#157, ADR 0005) — the 13 gates + `Mux` the opt-in
/// projection surfaces from a dissolved combinational block.
fn is_gate_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::And
            | NodeKind::Or
            | NodeKind::Xor
            | NodeKind::Xnor
            | NodeKind::Nand
            | NodeKind::Nor
            | NodeKind::Not
            | NodeKind::Buf
            | NodeKind::Add
            | NodeKind::Sub
            | NodeKind::Mul
            | NodeKind::Cmp
            | NodeKind::Shift
            | NodeKind::Mux
            | NodeKind::Concat
    )
}

/// Whether `id` is a gate-level primitive node.
fn is_gate(design: &Design, id: NodeId) -> bool {
    design.node(id).is_some_and(|n| is_gate_kind(n.kind))
}

/// The single operand a `Not` primitive inverts (the node driving its `in` edge).
fn not_operand(design: &Design, not_id: NodeId) -> Option<NodeId> {
    design
        .edges_of(not_id)
        .iter()
        .find(|e| e.port == not_id && e.dir != Dir::Out)
        .map(|e| e.endpoint)
}

/// Whether a `Not` primitive should be folded into an inversion bubble on its
/// consumer's input pin (#157) rather than drawn as a separate inverter box. True
/// only for a non-root inverter (no `out` edge — a pure sub-expression) whose
/// output feeds exactly one gate input, i.e. `a & ~b`. A `~signal` that drives a
/// scope signal directly (`assign y = ~a`) has an `out` edge and stays a Not box.
fn is_foldable_not(design: &Design, id: NodeId) -> bool {
    if design.node(id).map(|n| n.kind) != Some(NodeKind::Not) {
        return false;
    }
    let incident = design.edges_of(id);
    // A root inverter drives a scope signal via an `out` edge — keep it visible.
    if incident.iter().any(|e| e.port == id && e.dir == Dir::Out) {
        return false;
    }
    // The Not must have exactly one reader and it must be a gate (single fan-out
    // into one operator) — so folding it away never drops another consumer.
    let readers: Vec<_> = incident
        .iter()
        .filter(|e| e.endpoint == id && e.dir == Dir::In)
        .collect();
    readers.len() == 1 && is_gate(design, readers[0].port) && not_operand(design, id).is_some()
}

/// The one gate that reads a foldable inverter — the box its bubble is drawn on.
/// Meaningful only for an id [`is_foldable_not`] accepts, which is what
/// guarantees the reader exists and is unique.
fn not_reader(design: &Design, id: NodeId) -> Option<NodeId> {
    design
        .edges_of(id)
        .iter()
        .find(|e| e.endpoint == id && e.dir == Dir::In)
        .map(|e| e.port)
}

/// A node neither view draws as a box of its own, because it renders *inside*
/// the box that consumes it: a literal tie, carried inline on the consumer's
/// `SchPort.constant` (#199), and a folded inverter, drawn as an `Inv` bubble on
/// its consumer's input pin (#157). `child_boxes` makes a box for neither.
fn is_drawn_inline(design: &Design, id: NodeId, projection: Projection) -> bool {
    is_kind(design, id, NodeKind::Const)
        || (projection == Projection::GateLevel && is_foldable_not(design, id))
}

/// Resolve a gate-input edge endpoint through a foldable `Not` (#157): returns the
/// un-inverted operand and `true` when the endpoint is a folded inverter, else the
/// endpoint unchanged and `false`. Used identically by `make_gate_box` (pin build)
/// and the signal-join pass (wiring key) so the pin id and its wire agree.
fn fold_inverter(design: &Design, endpoint: NodeId) -> (NodeId, bool) {
    if is_foldable_not(design, endpoint) {
        if let Some(op) = not_operand(design, endpoint) {
            return (op, true);
        }
    }
    (endpoint, false)
}

/// Last segment of a path (used to label unnamed generate blocks).
fn last_segment(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// Path relative to `scope` (`scope.a.b` → `a.b`); falls back to the last
/// segment, then the whole path. Used to label wires with their net name.
fn relative_to(path: &str, scope: &str) -> String {
    path.strip_prefix(scope)
        .and_then(|s| s.strip_prefix('.'))
        .map(str::to_string)
        .unwrap_or_else(|| last_segment(path).to_string())
}

/// Append an edge's resolved bit-select to a net label (`core_trap` + `[0]` →
/// `core_trap[0]`), so a vector fanned out per bit shows which bit each wire
/// carries. Falls back to the bare label when the edge connects the whole signal.
fn with_select(base: String, select: &Option<String>) -> String {
    match select {
        Some(s) => format!("{base}{s}"),
        None => base,
    }
}

/// The module/definition type of an instance, taken from the basename (no
/// extension) of the file that defines it (`…/picorv32.v` → `picorv32`). This is
/// a best-effort recovery until the harness emits the real definition name.
/// Public so the GUI's hierarchy tree can sublabel its nodes the same way the
/// schematic sublabels its boxes.
pub fn module_of(design: &Design, node: &svxprobe_model::Node) -> Option<String> {
    // Module instances and interface instances both carry a defining-file sublabel
    // (e.g. `picorv32`, `mem_if`); a consuming interface port has no def_range and
    // falls through to `None`.
    if !matches!(node.kind, NodeKind::Instance | NodeKind::Interface) {
        return None;
    }
    let file = node.def_range?.file;
    let path = &design.doc.files.iter().find(|f| f.id == file)?.path;
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    Some(
        base.rsplit_once('.')
            .map(|(s, _)| s)
            .unwrap_or(base)
            .to_string(),
    )
}

/// The declared bit-range of a port (`logic[31:0]` → `[31:0]`), or `None` for a
/// scalar. Used to annotate pins.
fn width_of(type_: &Option<String>) -> Option<String> {
    let t = type_.as_ref()?;
    let lo = t.find('[')?;
    let hi = t[lo..].find(']')? + lo;
    Some(t[lo..=hi].to_string())
}

/// Pin bit-range with an enum fallback (#118): the declared packed range, or —
/// for an enum-typed signal (`lane_state_e`) — the range implied by the enum's
/// width in the model's normalized enum table. A model fact, never a guess.
///
/// Public so the GUI's waveform signal picker (#171) annotates its rows exactly as
/// the schematic annotates pins — one width rule, no divergence. Same reason
/// `is_bare_interface`/`module_of` are public.
pub fn pin_width(design: &Design, type_: &Option<String>) -> Option<String> {
    width_of(type_).or_else(|| {
        let e = design.enum_for_type(type_.as_deref()?)?;
        Some(format!("[{}:0]", e.width.saturating_sub(1)))
    })
}

/// Aggregate access ports of a bare interface instance bundle (#96): one port
/// per way the design touches the bundle, read off the connection edges —
/// never a name guess. Returns the used modport views as `(Modport node, view
/// name, side)` — a consumer bound through a view (`.bus(bus.mem)`) gets a
/// port carried by the `Modport` node itself, so a click cross-probes the view
/// — plus the side of one shared raw port when consumers tap members directly
/// (`bus.valid`), or `None` when nothing does. Sides come from direction
/// majorities: a mostly-out view produces into the design (west), and the raw
/// port faces its tap majority (mostly driven-into ⇒ west).
fn access_ports(design: &Design, iface: NodeId) -> (Vec<(NodeId, String, Side)>, Option<Side>) {
    let Some(n) = design.node(iface) else {
        return (Vec::new(), None);
    };
    let inside: std::collections::HashSet<NodeId> = n.children.iter().copied().collect();
    // The modport view (name) an edge endpoint accesses the bundle through: the
    // consumer's modport-qualified interface port, or one of its member pins.
    let view_of = |id: NodeId| -> Option<&str> {
        let x = design.node(id)?;
        if x.kind == NodeKind::Interface {
            return x.modport.as_deref();
        }
        let p = design.node(x.parent?)?;
        if p.kind != NodeKind::Interface {
            return None;
        }
        p.modport.as_deref()
    };
    let mut used: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let (mut raw_in, mut raw_out) = (0usize, 0usize);
    // Only edges incident on the bundle or its interior can touch it — gather
    // those via conn_index instead of scanning every edge. BTreeSet dedups the
    // port/endpoint dual-listing so a raw port's tap is counted once.
    let mut cand: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    cand.extend(design.edge_indices_of(iface).iter().copied());
    for &c in &n.children {
        cand.extend(design.edge_indices_of(c).iter().copied());
    }
    for &ci in &cand {
        let e = &design.edges()[ci as usize];
        // A connection *into* the bundle: its endpoint is the instance, one of
        // its members, or a modport view — from a port outside the interface.
        if (e.endpoint != iface && !inside.contains(&e.endpoint))
            || e.port == iface
            || inside.contains(&e.port)
        {
            continue;
        }
        // A gate primitive's tap is an internal detail of a dissolved combinational
        // block, not a structural connection to the bundle — and it only exists when
        // the model was elaborated with the opt-in `--gate-level` pass. Counting it
        // would let that pass move a pin in the *process-level* graph, which draws no
        // gates at all (#215: `if`/`else` muxes reading `bus.ready`/`bus.rdata`
        // outvoted the real drivers and flipped the raw port east).
        if is_gate(design, e.port) {
            continue;
        }
        match view_of(e.port) {
            Some(v) => {
                used.insert(v);
            }
            None => match e.dir {
                Dir::Out => raw_out += 1,
                _ => raw_in += 1,
            },
        }
    }
    let side_of_view = |mp: &svxprobe_model::Node| -> Side {
        let ms = mp.members.as_deref().unwrap_or_default();
        let outs = ms.iter().filter(|m| m.dir == Dir::Out).count();
        if outs * 2 > ms.len() {
            Side::West
        } else {
            Side::East
        }
    };
    let views = n
        .children
        .iter()
        .filter_map(|&c| design.node(c))
        .filter(|c| c.kind == NodeKind::Modport && used.contains(c.name.as_str()))
        .map(|c| (c.id, c.name.clone(), side_of_view(c)))
        .collect();
    let raw = (raw_in + raw_out > 0).then_some(if raw_out >= raw_in {
        Side::West
    } else {
        Side::East
    });
    (views, raw)
}

/// Build a box node with its ports; port sides come from incident edges. The
/// label is scope-relative (like wire labels) so flattened generate-block
/// iterations stay distinct — e.g. `g_lane[0].core` vs `g_lane[1].core`.
fn make_box(design: &Design, bx: NodeId, scope: &str) -> Option<SchNode> {
    let n = design.node(bx)?;
    // A modport-qualified interface port carries directional member pins; the
    // bundle is the consumer's boundary to the outside, so its pin sides are
    // mirrored like the scope's own boundary pins (see `boundary_side`).
    let mirror = n.kind == NodeKind::Interface && n.modport.is_some();
    // Generate blocks have no ports; instances expose their Port children.
    let mut ports: Vec<SchPort> = n
        .children
        .iter()
        .copied()
        .filter(|&c| is_kind(design, c, NodeKind::Port))
        .map(|pid| {
            let node = design.node(pid);
            // A pin with no model edge and no constant tie-off is floating in
            // the design (#118): shown, but marked so the frontend dims it.
            let dangling = !design.edges_of(pid).iter().any(|e| e.port == pid)
                && node.is_some_and(|n| n.const_value.is_none());
            // Prefer the port's declared direction so even unconnected pins land
            // on the correct side; fall back to an incident edge, then West.
            let side = node
                .and_then(|n| n.dir)
                .or_else(|| {
                    design
                        .edges_of(pid)
                        .iter()
                        .find(|e| e.port == pid)
                        .map(|e| e.dir)
                })
                .map(if mirror { boundary_side } else { side_of })
                .unwrap_or(Side::West);
            SchPort {
                more: None,
                id: pid,
                name: node.map(|n| n.name.clone()).unwrap_or_default(),
                side,
                path: node.map(|n| n.path.clone()).unwrap_or_default(),
                width: node.and_then(|n| pin_width(design, &n.type_)),
                role: None,
                bundle: false,
                dangling,
                constant: None,
            }
        })
        .collect();
    // A bare interface instance aggregates its connections into access ports
    // (#96): one per consuming modport view — carried by the `Modport` node,
    // so wires and cross-probes anchor on the view — plus one raw port, named
    // after the interface type, fanning out to direct member taps.
    if n.kind == NodeKind::Interface && n.modport.is_none() {
        let (views, raw) = access_ports(design, bx);
        for (vid, name, side) in views {
            ports.push(SchPort {
                more: None,
                id: vid,
                name,
                side,
                path: design.node(vid).map(|x| x.path.clone()).unwrap_or_default(),
                width: None,
                role: None,
                bundle: true,
                dangling: false,
                constant: None,
            });
        }
        if let Some(side) = raw {
            ports.push(SchPort {
                more: None,
                id: RAW_PORT_BASE + bx,
                name: module_of(design, n).unwrap_or_else(|| n.name.clone()),
                side,
                path: n.path.clone(),
                width: None,
                role: None,
                bundle: true,
                dangling: false,
                constant: None,
            });
        }
    }
    // A modport-qualified interface port shows as a single bundle pin on the
    // consuming instance (#106) — the modport connection sits in the port row;
    // its members stay on the drilled view's bundle box. The pin id/path are
    // the interface-port node's own, so wires and cross-probes anchor to it.
    for &c in &n.children {
        let Some(cn) = design.node(c) else { continue };
        if cn.kind != NodeKind::Interface
            || cn.modport.is_none()
            || !design.edges_of(c).iter().any(|e| e.port == c)
        {
            continue;
        }
        // Side from the members' direction majority: a mostly-input view reads
        // like an input bundle (fall back west, like scalar pins).
        let (ins, outs) = cn
            .children
            .iter()
            .filter(|&&m| is_kind(design, m, NodeKind::Port))
            .fold((0, 0), |(i, o), &m| {
                match design.node(m).and_then(|x| x.dir) {
                    Some(Dir::Out) => (i, o + 1),
                    Some(_) => (i + 1, o),
                    None => (i, o),
                }
            });
        let side = if outs > ins { Side::East } else { Side::West };
        let view = cn.modport.as_deref().unwrap_or_default();
        let name = match module_of(design, cn) {
            Some(t) => format!("{} ({t}.{view})", cn.name),
            None => format!("{} ({view})", cn.name),
        };
        ports.push(SchPort {
            more: None,
            id: c,
            name,
            side,
            path: cn.path.clone(),
            width: None,
            role: None,
            bundle: true,
            dangling: false,
            constant: None,
        });
    }
    let label = relative_to(&n.path, scope);
    Some(SchNode {
        parent: None,
        id: bx,
        kind: n.kind,
        label,
        path: n.path.clone(),
        // A module instance always has an interior (its module body), so it is
        // always drillable — even a leaf with no child boxes (drilling renders its
        // I/O frame). A bare interface bundle drills into its modport views (#97).
        // Other box kinds are drillable only if they contain boxes.
        expandable: n.kind == NodeKind::Instance
            || is_bare_interface(design, bx)
            // Drillability is structural (does it contain boxes), independent of
            // the gate/process projection, so always ask at the process level.
            || !child_boxes(design, bx, Projection::ProcessLevel).is_empty(),
        ports,
        module: module_of(design, n),
        constant: None,
        modport: n.modport.clone(),
        mem_depth: None,
        init_source: None,
    })
}

/// A constant-source node driving one tied input `port` with literal `lit`. It
/// lays out (via the boundary-pin path) as a small source left of the consumer,
/// wired to the input — so ELK routes other wires around it (no crossings).
fn make_const_node(lit: &str, port: NodeId) -> SchNode {
    let id = CONST_ID_BASE + port;
    SchNode {
        parent: None,
        id,
        kind: NodeKind::Port,
        label: lit.to_string(),
        path: String::new(),
        expandable: false,
        // East-facing pin: drives rightward into the design.
        ports: vec![SchPort {
            more: None,
            id,
            name: lit.to_string(),
            side: Side::East,
            path: String::new(), // synthetic constant source — no model node
            width: None,
            role: None,
            bundle: false,
            dangling: false,
            constant: None,
        }],
        module: None,
        constant: Some(lit.to_string()),
        modport: None,
        mem_depth: None,
        init_source: None,
    }
}

/// An inferred register box. The FF has no model `Port` children, so synthesize a
/// pin per wired signal (clock/data on the west, Q on the east) from its edges;
/// pin ids come from `pins` keyed by `(ff, signal)` so wires can target them and
/// FFs sharing a net (e.g. clk) keep distinct pins.
fn make_ff_box(design: &Design, ff: NodeId, scope: &str, pins: &mut PinAlloc) -> Option<SchNode> {
    let n = design.node(ff)?;
    let ports: Vec<SchPort> = design
        .edges_of(ff)
        .iter()
        .filter(|e| e.port == ff)
        .map(|e| {
            let sig = design.node(e.endpoint);
            // Role from the model facts on the FF node (#59): `reset` names the
            // async-reset signal's canonical path, `type_` the clock signal's
            // name — both emitted by the harness, never guessed from pin names.
            let role = if e.dir != Dir::Out && sig.map(|s| s.path.as_str()) == n.reset.as_deref() {
                Some(PinRole::Reset)
            } else if e.dir != Dir::Out && sig.map(|s| s.name.as_str()) == n.type_.as_deref() {
                Some(PinRole::Clk)
            } else {
                None
            };
            SchPort {
                more: None,
                id: pins.pin(ff, e.endpoint),
                name: sig.map(|s| s.name.clone()).unwrap_or_default(),
                side: side_of(e.dir),
                path: sig.map(|s| s.path.clone()).unwrap_or_default(),
                width: sig.and_then(|s| pin_width(design, &s.type_)),
                role,
                bundle: false,
                dangling: false,
                constant: None,
            }
        })
        .collect();
    Some(SchNode {
        parent: None,
        id: ff,
        kind: NodeKind::Ff,
        label: if n.name.is_empty() {
            "FF".into()
        } else {
            relative_to(&n.path, scope)
        },
        path: n.path.clone(),
        expandable: false,
        ports,
        module: None,
        constant: None,
        modport: None,
        mem_depth: None,
        init_source: None,
    })
}

/// A combinational-logic box — an `always_comb`/`always @*` process (`Comb`) or a
/// continuous `assign` (`Assign`). Like an FF it has no model `Port` children, so
/// synthesize a pin per wired signal (reads on the west, assigns on the east) from
/// its edges; pin ids come from `pins` keyed by `(box, signal)` so boxes sharing a
/// net keep distinct pins. The kind is carried through so the frontend can draw a
/// process box vs an assign function node.
fn make_logic_box(design: &Design, bx: NodeId, pins: &mut PinAlloc) -> Option<SchNode> {
    let n = design.node(bx)?;
    let ports: Vec<SchPort> = design
        .edges_of(bx)
        .iter()
        .filter(|e| e.port == bx)
        .map(|e| {
            let sig = design.node(e.endpoint);
            // A latch's `enable` model fact (#59) names the gating signal's
            // canonical path; tag the matching input pin.
            let role = (n.kind == NodeKind::Latch
                && e.dir != Dir::Out
                && sig.map(|s| s.path.as_str()) == n.enable.as_deref())
            .then_some(PinRole::Enable);
            SchPort {
                more: None,
                id: pins.pin(bx, e.endpoint),
                name: sig.map(|s| s.name.clone()).unwrap_or_default(),
                side: side_of(e.dir),
                path: sig.map(|s| s.path.clone()).unwrap_or_default(),
                width: sig.and_then(|s| pin_width(design, &s.type_)),
                role,
                bundle: false,
                dangling: false,
                constant: None,
            }
        })
        .collect();
    let label = match n.kind {
        NodeKind::Assign => "assign",
        NodeKind::Latch => "latch",
        _ => "comb",
    };
    Some(SchNode {
        parent: None,
        id: bx,
        kind: n.kind,
        label: label.into(),
        path: n.path.clone(),
        expandable: false,
        ports,
        module: None,
        constant: None,
        modport: None,
        mem_depth: None,
        init_source: None,
    })
}

/// A memory-array box (#112). Like an FF, a `Memory` node has no model `Port`
/// children, so synthesize a pin per memory-access edge (`edges_of(mem)` carrying
/// a `mem_port` role): the address and write-data enter on the west, the read-data
/// leaves on the east. Pin ids come from `pins` keyed by `(mem, signal)` so the
/// signal-join pass wires them to the real scope signals (`word_idx`/`wdata`/
/// `rdata`). Carries `mem_depth` + `init_source` so the frontend labels the array
/// and shows the INIT marker.
fn make_memory_box(
    design: &Design,
    mem: NodeId,
    scope: &str,
    pins: &mut PinAlloc,
) -> Option<SchNode> {
    let n = design.node(mem)?;
    // One pin per accessed signal; the harness already dedups byte-lane writes,
    // but guard against a repeat endpoint collapsing onto a duplicate pin id.
    let mut seen: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    let ports: Vec<SchPort> = design
        .edges_of(mem)
        .iter()
        .filter(|e| e.port == mem && e.mem_port.is_some() && seen.insert(e.endpoint))
        .map(|e| {
            let sig = design.node(e.endpoint);
            let role = match e.mem_port {
                Some(MemPort::Addr) => Some(PinRole::Addr),
                Some(MemPort::Din) => Some(PinRole::Din),
                Some(MemPort::Dout) => Some(PinRole::Dout),
                None => None,
            };
            SchPort {
                more: None,
                id: pins.pin(mem, e.endpoint),
                name: sig.map(|s| s.name.clone()).unwrap_or_default(),
                side: side_of(e.dir),
                path: sig.map(|s| s.path.clone()).unwrap_or_default(),
                width: sig.and_then(|s| pin_width(design, &s.type_)),
                role,
                bundle: false,
                dangling: false,
                constant: None,
            }
        })
        .collect();
    Some(SchNode {
        parent: None,
        id: mem,
        kind: NodeKind::Memory,
        label: relative_to(&n.path, scope),
        path: n.path.clone(),
        expandable: false,
        ports,
        module: None,
        constant: None,
        modport: None,
        mem_depth: n.mem_depth,
        init_source: n.init_source.clone(),
    })
}

/// A gate-level primitive box (#157, ADR 0005): an `And`/`Or`/…/`Cmp`/`Mux`
/// surfaced when a combinational block is dissolved. Like an FF it has no model
/// `Port` children, so synthesize an input pin per operand edge (west) plus a
/// single output pin (east). The output pin is keyed `(gate, gate)` — a gate has
/// at most one consumer (a scope signal or another gate), and both wire through
/// the signal-join pass, which drives key `gate` from this pin. A `Mux`'s select
/// input carries `PinRole::Sel` (from its `MuxPort::Sel` edge role) so the
/// frontend can place it on the trapezoid's south wall.
fn make_gate_box(design: &Design, gate: NodeId, pins: &mut PinAlloc) -> Option<SchNode> {
    let n = design.node(gate)?;
    let mut ports: Vec<SchPort> = design
        .edges_of(gate)
        .iter()
        .filter(|e| e.port == gate && e.dir != Dir::Out)
        .map(|e| {
            // Fold a single-fanout `~operand` inverter into a bubble on this pin:
            // the pin then represents the un-inverted operand (its `path`/width),
            // tagged `Inv` so the frontend draws the bubble. The producer `Not` is
            // dropped from `child_boxes`, and the signal-join keys this load on the
            // same folded endpoint.
            let (endpoint, inverted) = fold_inverter(design, e.endpoint);
            let sig = design.node(endpoint);
            // Sel wins the role slot (a mux select is never a folded inverter); an
            // inverted operand is tagged `Inv`; other data branches stay plain.
            let role = if matches!(e.mux_port, Some(MuxPort::Sel)) {
                Some(PinRole::Sel)
            } else if inverted {
                Some(PinRole::Inv)
            } else {
                None
            };
            SchPort {
                more: None,
                id: pins.pin(gate, endpoint),
                name: sig.map(|s| s.name.clone()).unwrap_or_default(),
                side: Side::West,
                path: sig.map(|s| s.path.clone()).unwrap_or_default(),
                width: sig.and_then(|s| pin_width(design, &s.type_)),
                role,
                bundle: false,
                dangling: false,
                // A constant/parameter operand carries its value inline (#199).
                constant: sig.and_then(|s| s.const_value.clone()),
            }
        })
        .collect();
    // The single result pin, keyed on the gate itself so gate→gate and gate→signal
    // wires resolve to it in the signal-join pass. Carries the gate's own path so a
    // right-click cross-probes to its sub-expression `def_range`.
    ports.push(SchPort {
        more: None,
        id: pins.pin(gate, gate),
        name: String::new(),
        side: Side::East,
        path: n.path.clone(),
        width: None,
        role: None,
        bundle: false,
        dangling: false,
        constant: None,
    });
    Some(SchNode {
        parent: None,
        id: gate,
        kind: n.kind,
        label: gate_label(n),
        path: n.path.clone(),
        expandable: false,
        ports,
        module: None,
        constant: None,
        modport: None,
        mem_depth: None,
        init_source: None,
    })
}

/// The label for a gate box: a datapath primitive (`Add`/`Sub`/`Mul`/`Cmp`/`Shift`)
/// keeps its exact operator (`op`, e.g. `LessThan`) — the coarse kind can't tell
/// `<` from `==`; the bitwise/reduction gates are named by their kind (`and`,
/// `mux`, …). The frontend draws the real IEEE glyph over this fallback (PR5).
fn gate_label(n: &Node) -> String {
    n.op.clone().unwrap_or_else(|| n.name.clone())
}

/// A boundary I/O pin for one of the scope's own ports. Rendered as a frame pin
/// (no box). Its single pin faces the design — inputs enter from the west frame
/// (pin on the east), outputs leave at the east frame (pin on the west) — so the
/// layered layout places inputs on the left and outputs on the right.
fn make_boundary_pin(design: &Design, port: NodeId) -> Option<SchNode> {
    let n = design.node(port)?;
    let side = boundary_side(n.dir.unwrap_or(Dir::In));
    Some(SchNode {
        parent: None,
        id: port,
        kind: NodeKind::Port,
        label: n.name.clone(),
        path: n.path.clone(),
        expandable: false,
        ports: vec![SchPort {
            more: None,
            id: port,
            name: n.name.clone(),
            side,
            path: n.path.clone(),
            width: pin_width(design, &n.type_),
            role: None,
            bundle: false,
            dangling: false,
            constant: None,
        }],
        module: None,
        constant: None,
        modport: None,
        mem_depth: None,
        init_source: None,
    })
}

/// The interior of a bare interface bundle (#97): each `modport` view as a box
/// (one directional pin per member), the interface's own ports (e.g. `clk`) as
/// boundary frame pins, and a boundary frame port per view marking its external
/// face. Wires connect every member one view drives and another reads, plus the
/// interface's own inputs into the views that read them (clk into both). Each
/// member pin resolves to its underlying bundle signal via
/// [`Design::modport_member_nodes`], so its `path` — and any wire's `net_path` —
/// cross-probes the real signal. No heuristics; the modport membership is a model
/// fact from the harness.
fn interface_interior(design: &Design, iface: NodeId, scope_path: &str) -> SchematicGraph {
    let Some(ifnode) = design.node(iface) else {
        return SchematicGraph {
            truncated: false,
            root: scope_path.to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
        };
    };
    let views: Vec<NodeId> = ifnode
        .children
        .iter()
        .copied()
        .filter(|&c| is_kind(design, c, NodeKind::Modport))
        .collect();
    let own_ports: Vec<NodeId> = ifnode
        .children
        .iter()
        .copied()
        .filter(|&c| is_kind(design, c, NodeKind::Port))
        .collect();

    let mut pins = PinAlloc::new();
    let mut nodes: Vec<SchNode> = Vec::new();
    // Underlying member signal -> the pins that drive / read it, so a signal one
    // view outputs and another inputs (or an interface input port feeding both
    // views) is wired between them. Keyed on the *first node at the member's
    // canonical path* so a modport member and the interface's own port for the
    // same signal (e.g. `clk`) share a bucket. BTreeMap keeps wires deterministic.
    let mut drivers: std::collections::BTreeMap<NodeId, Vec<NodeId>> = Default::default();
    let mut loads: std::collections::BTreeMap<NodeId, Vec<NodeId>> = Default::default();
    let key_of = |sig: NodeId| -> NodeId {
        design
            .node(sig)
            .and_then(|n| design.nodes_at_path(&n.path).first().copied())
            .unwrap_or(sig)
    };

    // 1. Each modport view as a box, one directional pin per member. Record each
    //    member's (pin, signal) so the view's frame port (step 4) can fan out to
    //    the exact I/O the modport lists rather than the box corner.
    let mut view_pins: std::collections::BTreeMap<NodeId, Vec<(NodeId, NodeId)>> =
        Default::default();
    for &mp in &views {
        let Some(mn) = design.node(mp) else { continue };
        let members = mn.members.as_deref().unwrap_or_default();
        let mut ports = Vec::with_capacity(members.len());
        for m in members {
            // The member's own signal node in the bundle (path lookup, not a
            // guess); skip a member that doesn't resolve rather than fabricate a
            // colliding synthetic pin. The pin id is per (view, signal) so the
            // two views that share a member keep distinct pins.
            let Some(sig) = design.modport_member_nodes(mp, &m.name).first().copied() else {
                continue;
            };
            let signode = design.node(sig);
            let pin = pins.pin(mp, sig);
            ports.push(SchPort {
                more: None,
                id: pin,
                name: m.name.clone(),
                side: side_of(m.dir),
                path: signode.map(|s| s.path.clone()).unwrap_or_default(),
                width: signode.and_then(|s| pin_width(design, &s.type_)),
                role: None,
                bundle: false,
                dangling: false,
                constant: None,
            });
            view_pins.entry(mp).or_default().push((pin, sig));
            let key = key_of(sig);
            match m.dir {
                Dir::Out => drivers.entry(key).or_default().push(pin),
                Dir::In => loads.entry(key).or_default().push(pin),
                _ => {
                    drivers.entry(key).or_default().push(pin);
                    loads.entry(key).or_default().push(pin);
                }
            }
        }
        nodes.push(SchNode {
            parent: None,
            id: mp,
            kind: NodeKind::Modport,
            label: relative_to(&mn.path, scope_path),
            path: mn.path.clone(),
            expandable: false,
            ports,
            module: None,
            constant: None,
            modport: None,
            mem_depth: None,
            init_source: None,
        });
    }

    // 2. The interface's own ports (e.g. `clk`) as boundary frame pins, folded by
    //    declared direction so an input port drives the member pins reading it.
    //    Keyed the same way as the members so clk reaches both views' clk pins;
    //    `own_keys` marks those signals so step 4 leaves them off the frame fan
    //    (clk is already wired to its own boundary pin).
    let mut own_keys: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    for &p in &own_ports {
        nodes.extend(make_boundary_pin(design, p));
        let key = key_of(p);
        own_keys.insert(key);
        match design.node(p).and_then(|n| n.dir) {
            Some(Dir::Out) => loads.entry(key).or_default().push(p),
            Some(Dir::In) => drivers.entry(key).or_default().push(p),
            _ => {
                drivers.entry(key).or_default().push(p);
                loads.entry(key).or_default().push(p);
            }
        }
    }

    // 3. Cross drivers x loads into wires. `seen` (min,max) dedups a pair so an
    //    inout member shared by two views draws one wire, not two.
    let mut edges = Vec::new();
    let mut next_edge = 0u32;
    let mut seen: std::collections::HashSet<(NodeId, NodeId)> = std::collections::HashSet::new();
    for (&sig, ds) in &drivers {
        let Some(ls) = loads.get(&sig) else { continue };
        let signode = design.node(sig);
        let net = signode.map(|n| relative_to(&n.path, scope_path));
        let net_path = signode.map(|n| n.path.clone());
        for &d in ds {
            for &l in ls {
                if d == l || !seen.insert((d.min(l), d.max(l))) {
                    continue;
                }
                edges.push(SchEdge {
                    id: next_edge,
                    source: d,
                    target: l,
                    net: net.clone(),
                    net_path: net_path.clone(),
                });
                next_edge += 1;
            }
        }
    }

    // 4. A boundary frame port per modport view (its external face), fanned out to
    //    the view's own member pins — the inputs and outputs the modport lists — so
    //    the frame connects to the I/O rows, not the box corner. A member backed by
    //    an interface port (clk) is already wired to that boundary pin, so it is
    //    left off the fan. A mostly-driving view sits on the west frame, a
    //    mostly-reading one on the east, so the internal core->mem flow reads
    //    left-to-right.
    for &mp in &views {
        let Some(mn) = design.node(mp) else { continue };
        let members = mn.members.as_deref().unwrap_or_default();
        let outs = members.iter().filter(|m| m.dir == Dir::Out).count();
        let side = if outs * 2 > members.len() {
            Side::East
        } else {
            Side::West
        };
        let fid = MODPORT_FRAME_BASE + mp;
        nodes.push(SchNode {
            parent: None,
            id: fid,
            kind: NodeKind::Port,
            label: mn.name.clone(),
            path: mn.path.clone(),
            expandable: false,
            ports: vec![SchPort {
                more: None,
                id: fid,
                name: mn.name.clone(),
                side,
                path: mn.path.clone(),
                width: None,
                role: None,
                bundle: true,
                dangling: false,
                constant: None,
            }],
            module: None,
            constant: None,
            modport: None,
            mem_depth: None,
            init_source: None,
        });
        for &(pin, sig) in view_pins.get(&mp).map(Vec::as_slice).unwrap_or_default() {
            if own_keys.contains(&key_of(sig)) {
                continue;
            }
            let signode = design.node(sig);
            edges.push(SchEdge {
                id: next_edge,
                source: fid,
                target: pin,
                net: signode.map(|n| relative_to(&n.path, scope_path)),
                net_path: signode.map(|n| n.path.clone()),
            });
            next_edge += 1;
        }
    }

    SchematicGraph {
        truncated: false,
        root: scope_path.to_string(),
        nodes,
        edges,
    }
}

/// Which box a node in a scope belongs to, and which pin an edge endpoint
/// anchors on — the scope's whole anchoring vocabulary, built once.
///
/// Consulted by **both** views so they cannot disagree about the same network
/// (#268/#269): [`scope_graph_with`] resolves every edge through
/// [`Self::resolve`], and [`cone_with`] — which reaches boxes across many scopes
/// and picks each one itself — resolves the pin through [`Self::pin_in_box`],
/// the half of `resolve` that stays true once the box is already known.
///
/// Deliberately keyed on the *scope*, never on the subset of boxes a caller
/// happens to have reached: `resolve`'s bundle-member rule anchors only when
/// **one** bundle views the member, and that arity test gives a different answer
/// against a subset. A cone caching one of these per scope gets the scope
/// graph's answer for free; one built from its own emitted set would not.
struct ScopeAnchors {
    /// [`child_boxes`] in model order — pin allocation follows it.
    boxes: Vec<NodeId>,
    box_set: std::collections::HashSet<NodeId>,
    /// The scope's own ports, drawn as boundary frame pins.
    own_ports: Vec<NodeId>,
    /// Each own port — and its same-path backing net/var — to the frame pin it
    /// draws on.
    boundary_of: std::collections::HashMap<NodeId, NodeId>,
    /// Bundle member -> [(bundle box, pin)]. Several bundles in one scope can
    /// view the same member, hence the `Vec`; `BTreeMap` keeps the signal-join
    /// fold deterministic.
    iface_pin: std::collections::BTreeMap<NodeId, Vec<(NodeId, NodeId)>>,
    /// A bare interface bundle and its non-`Port` interior, to the owning
    /// bundle box. `BTreeMap` for the same reason as `iface_pin`.
    iface_owner: std::collections::BTreeMap<NodeId, NodeId>,
    /// A bare bundle to its aggregate raw access port (#96), when anything taps
    /// its members raw. Point-queried only, so a `HashMap` is enough.
    raw_port: std::collections::HashMap<NodeId, NodeId>,
}

impl ScopeAnchors {
    fn for_scope(design: &Design, scope: NodeId, projection: Projection) -> Self {
        let boxes = child_boxes(design, scope, projection);
        let box_set: std::collections::HashSet<NodeId> = boxes.iter().copied().collect();

        // Boundary I/O: the scope's *own* ports, drawn as frame pins (inputs left,
        // outputs right). An edge that reaches such a port — or its same-path backing
        // net/var — anchors to that pin so the connection is visible.
        let own_ports: Vec<NodeId> = design
            .node(scope)
            .map(|n| {
                n.children
                    .iter()
                    .copied()
                    .filter(|&c| is_kind(design, c, NodeKind::Port))
                    .collect()
            })
            .unwrap_or_default();
        let mut boundary_of: std::collections::HashMap<NodeId, NodeId> =
            std::collections::HashMap::new();
        if let Some(scope_node) = design.node(scope) {
            for &p in &own_ports {
                boundary_of.insert(p, p);
                let ppath = design.node(p).map(|n| n.path.as_str());
                for &sib in &scope_node.children {
                    if sib != p && design.node(sib).map(|n| n.path.as_str()) == ppath {
                        boundary_of.insert(sib, p);
                    }
                }
            }
        }

        // Modport member pins: an in-scope modport-qualified interface port pins
        // its bundle members, and each pin's edge points at the underlying member —
        // a signal that lives *outside* this scope (in the interface instance), so
        // `box_of` can never anchor it. Map member -> [(bundle box, pin)] so wires
        // to bundle signals land on the pins instead of being dropped.
        let mut iface_pin: std::collections::BTreeMap<NodeId, Vec<(NodeId, NodeId)>> =
            std::collections::BTreeMap::new();
        for &b in &boxes {
            let Some(bn) = design.node(b) else { continue };
            if bn.kind != NodeKind::Interface || bn.modport.is_none() {
                continue;
            }
            for &pid in &bn.children {
                if !is_kind(design, pid, NodeKind::Port) {
                    continue;
                }
                for e in design.edges_of(pid) {
                    if e.port == pid {
                        iface_pin.entry(e.endpoint).or_default().push((b, pid));
                    }
                }
            }
        }

        // Aggregate access ports on bare interface bundles (#96): map the instance
        // and its non-Port interior (members, modport views) to the owning bundle,
        // so edges reaching any of them anchor on the bundle's raw port (member
        // taps fan out one wire per consumer pin); modport-level connections are
        // retargeted onto the matching view's port by [`Self::view_port`].
        let mut iface_owner: std::collections::BTreeMap<NodeId, NodeId> =
            std::collections::BTreeMap::new();
        let mut raw_port: std::collections::HashMap<NodeId, NodeId> =
            std::collections::HashMap::new();
        for &b in &boxes {
            let Some(bn) = design.node(b) else { continue };
            if bn.kind != NodeKind::Interface || bn.modport.is_some() {
                continue;
            }
            iface_owner.insert(b, b);
            for &c in &bn.children {
                // Port children stay real pins with their own edges (e.g. clk).
                if !is_kind(design, c, NodeKind::Port) {
                    iface_owner.insert(c, b);
                }
            }
            if access_ports(design, b).1.is_some() {
                raw_port.insert(b, RAW_PORT_BASE + b);
            }
        }

        Self {
            boxes,
            box_set,
            own_ports,
            boundary_of,
            iface_pin,
            iface_owner,
            raw_port,
        }
    }

    /// The frame pin a scope-boundary node draws on, if it is one.
    #[inline]
    fn boundary_pin(&self, node: NodeId) -> Option<NodeId> {
        self.boundary_of.get(&node).copied()
    }

    /// The pin `node` anchors on **within a box already known to own it** — the
    /// part of [`Self::resolve`] that does not depend on how the box was picked,
    /// and therefore the part a cone can share.
    ///
    /// Draws to the specific pin when the node is a `Port` directly under the
    /// box; a bare bundle's interior anchors on its raw access port (#96, falling
    /// back to the box when nothing taps members raw); a modport-qualified
    /// interface port under the box — or one of its member pins — anchors on the
    /// box's bundle pin (#106), collapsing the port-level and member-level edges
    /// onto one anchor; otherwise the box itself.
    #[inline]
    fn pin_in_box(&self, design: &Design, node: NodeId, b: NodeId) -> NodeId {
        let bundle_pin = |id: NodeId| {
            design.node(id).and_then(|n| {
                (n.kind == NodeKind::Interface && n.modport.is_some() && n.parent == Some(b))
                    .then_some(id)
            })
        };
        if is_kind(design, node, NodeKind::Port)
            && design.node(node).and_then(|n| n.parent) == Some(b)
        {
            node
        } else if let Some(owner) = self.iface_owner.get(&node) {
            *self.raw_port.get(owner).unwrap_or(owner)
        } else if let Some(p) = bundle_pin(node).or_else(|| {
            design
                .node(node)
                .filter(|n| n.kind == NodeKind::Port)
                .and_then(|n| n.parent)
                .and_then(bundle_pin)
        }) {
            p
        } else {
            b
        }
    }

    /// Map an edge endpoint to (box-in-scope, endpoint-to-draw).
    #[inline]
    fn resolve(&self, design: &Design, node: NodeId) -> Option<(NodeId, NodeId)> {
        if let Some(bp) = self.boundary_pin(node) {
            return Some((bp, bp));
        }
        // Anchor a bundle member on its pin only when one bundle views it —
        // with several, a structural edge has no single right pin (each pin
        // still wires by direction via the signal-join fold).
        if let Some([(b, pin)]) = self.iface_pin.get(&node).map(Vec::as_slice) {
            return Some((*b, *pin));
        }
        let b = box_of(design, node, &self.box_set)?;
        Some((b, self.pin_in_box(design, node, b)))
    }

    /// A modport-level connection anchors on the bundle's view port (#96): when
    /// the far end is a modport-qualified bundle pin, the bare-interface end
    /// moves from its raw/box anchor onto the port carried by the matching
    /// `Modport` node — so `.bus(bus.mem)` wires bundle pin to `mem`, directly.
    #[inline]
    fn view_port(&self, design: &Design, b: NodeId, far_pin: NodeId) -> Option<NodeId> {
        if self.iface_owner.get(&b) != Some(&b) {
            return None; // not a bare interface bundle in this scope
        }
        let view = design
            .node(far_pin)
            .filter(|x| x.kind == NodeKind::Interface)?
            .modport
            .as_deref()?;
        design.node(b)?.children.iter().copied().find(|&c| {
            design
                .node(c)
                .is_some_and(|m| m.kind == NodeKind::Modport && m.name == view)
        })
    }
}

/// The schematic of one scope at the process level (ADR 0004 default) — the bare
/// entry point every existing caller uses. Delegates to [`scope_graph_with`] with
/// [`Projection::ProcessLevel`], so its output is unchanged.
pub fn scope_graph(design: &Design, scope_path: &str) -> Option<SchematicGraph> {
    scope_graph_with(design, scope_path, Projection::ProcessLevel)
}

/// The schematic of one scope: child-instance boxes wired by the connections
/// whose two ends both live inside the scope. Connections that leave the scope
/// (e.g. to a top-level clock) are omitted here; use [`cone`] to trace those.
///
/// `projection` selects the internal-logic granularity (#157, ADR 0005): the
/// default [`Projection::ProcessLevel`] keeps one box per `always`/`assign`;
/// [`Projection::GateLevel`] dissolves each combinational block into its gate/mux
/// primitive network. A model with no gate primitives renders identically at both.
pub fn scope_graph_with(
    design: &Design,
    scope_path: &str,
    projection: Projection,
) -> Option<SchematicGraph> {
    // A logic-only generate block is not a scope (#184) — `is_navigable_scope` rejects
    // it so a cross-probe onto its path walks up to the enclosing module.
    let scope = *design
        .nodes_at_path(scope_path)
        .iter()
        .find(|&&id| is_navigable_scope(design, id))?;
    // A bare interface bundle drills into its modport views/members (#97), a
    // different projection than the instance-wiring path below.
    if is_bare_interface(design, scope) {
        return Some(interface_interior(design, scope, scope_path));
    }

    let anchors = ScopeAnchors::for_scope(design, scope, projection);
    // Synthesized pins (FF clk/data/Q) are handed out per (box, signal) so boxes
    // sharing a net keep distinct pins. The same allocator is reused by the FF
    // wiring branch below so wires resolve to the pins built here.
    let mut pins = PinAlloc::new();
    let mut nodes: Vec<SchNode> = Vec::new();
    for &b in &anchors.boxes {
        let node = match design.node(b).map(|n| n.kind) {
            Some(NodeKind::Ff) => make_ff_box(design, b, scope_path, &mut pins),
            Some(NodeKind::Comb) | Some(NodeKind::Latch) | Some(NodeKind::Assign) => {
                make_logic_box(design, b, &mut pins)
            }
            Some(NodeKind::Memory) => make_memory_box(design, b, scope_path, &mut pins),
            // Gate-level primitives (#157) only reach `boxes` under `GateLevel`,
            // where `child_boxes` dissolved their parent block into them.
            Some(k) if is_gate_kind(k) => make_gate_box(design, b, &mut pins),
            _ => make_box(design, b, scope_path),
        };
        nodes.extend(node);
    }

    // Boundary I/O: the scope's *own* ports, drawn as frame pins (inputs left,
    // outputs right). An edge that reaches such a port — or its same-path backing
    // net/var — anchors to that pin so the connection is visible (the mapping is
    // `anchors.boundary_of`, built with the rest of the scope's anchoring).
    nodes.extend(
        anchors
            .own_ports
            .iter()
            .filter_map(|&p| make_boundary_pin(design, p)),
    );

    // Gather the scope-local candidate edges once (the structural and signal-join
    // passes below share it). Every edge those passes act on has its `port`
    // incident on an in-scope box, one of a box's child ports, or the scope
    // boundary — so the union of incident edges over those nodes is a superset of
    // what a full scan would touch (extra candidates hit the same `continue`
    // guards). BTreeSet<u32> dedups the port/endpoint dual-listing AND preserves
    // ascending `doc.edges` position order, so edge ids, `seen` winners, pin
    // allocation, and the `next_edge` sequence stay byte-identical to the scan.
    let mut cand: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    {
        let mut seed: std::collections::HashSet<NodeId> = anchors.box_set.clone();
        for &b in &anchors.boxes {
            if let Some(bn) = design.node(b) {
                seed.extend(bn.children.iter().copied());
            }
        }
        seed.extend(anchors.boundary_of.keys().copied());
        for &id in &seed {
            cand.extend(design.edge_indices_of(id).iter().copied());
        }
    }

    let mut edges = Vec::new();
    let mut seen: std::collections::HashSet<(NodeId, NodeId)> = std::collections::HashSet::new();
    for &i in &cand {
        let e = &design.edges()[i as usize];
        // Logic boxes and gate primitives wire through the scope-level signals
        // (and gate-to-gate links) they read and assign; that is done by the
        // signal-join pass below. Here we draw only the structural (instance)
        // connections, both ends resolving to in-scope boxes.
        if is_logic_box(design, e.port) || is_gate(design, e.port) {
            continue;
        }
        if let (Some((sb, src)), Some((tb, tgt))) = (
            anchors.resolve(design, e.port),
            anchors.resolve(design, e.endpoint),
        ) {
            let (src, tgt) = (
                anchors.view_port(design, sb, tgt).unwrap_or(src),
                anchors.view_port(design, tb, src).unwrap_or(tgt),
            );
            // Collapse parallel connections that land on the same two anchors
            // (e.g. both lanes' clk meeting one boundary pin).
            if sb != tb && seen.insert((src.min(tgt), src.max(tgt))) {
                let endpoint = design.node(e.endpoint);
                let net =
                    endpoint.map(|n| with_select(relative_to(&n.path, scope_path), &e.select));
                let net_path = endpoint.map(|n| n.path.clone());
                edges.push(SchEdge {
                    id: i,
                    source: src,
                    target: tgt,
                    net,
                    net_path,
                });
            }
        }
    }
    let mut next_edge = design.edges().len() as u32;

    // Signal-join wiring: connect the scope's boxes through the scope-level
    // Vars/Nets the structural pass drops (it only resolves endpoints that sit
    // under a box). For each signal, cross its drivers (edges out) with its
    // loads (edges in) over the per-(box,signal) pins; the scope's own ports
    // and plain instance ports (#116) fold in — an input drives its signal, an
    // output loads it. Deliberately *not* gated on the scope having logic
    // boxes (#123): each fold self-gates on its inputs and `seen` dedups
    // against the structural pass, so in a logic-free hierarchical scope this
    // adds exactly the instance-to-instance wires mediated by a plain net.
    {
        // (pin id, resolved bit-select) for each end of a signal.
        type Anchor = (NodeId, Option<String>);
        let mut drivers: std::collections::HashMap<NodeId, Vec<Anchor>> =
            std::collections::HashMap::new();
        let mut loads: std::collections::HashMap<NodeId, Vec<Anchor>> =
            std::collections::HashMap::new();
        // Same scope-local candidate set as the structural pass (ascending edge
        // order preserved) — not a full scan.
        for &i in &cand {
            let e = &design.edges()[i as usize];
            // Join a boundary signal under its boundary pin so a port and its
            // backing net share a bucket; other signals key on the signal node.
            let key = anchors.boundary_pin(e.endpoint).unwrap_or(e.endpoint);
            if is_logic_box(design, e.port) && anchors.box_set.contains(&e.port) {
                let anchor = (pins.pin(e.port, e.endpoint), e.select.clone());
                if e.dir == Dir::Out {
                    drivers.entry(key).or_default().push(anchor);
                } else {
                    loads.entry(key).or_default().push(anchor);
                }
                continue;
            }
            // Gate primitives (#157) wire the same way, but a gate has a single
            // result pin keyed on itself (`make_gate_box`). Its `out` edge drives
            // the assigned signal (key = that signal); each `in` edge loads either
            // a scope signal or a producer gate (key = the endpoint), matched by
            // the producer's self-driver added below.
            if is_gate(design, e.port) && anchors.box_set.contains(&e.port) {
                if e.dir == Dir::Out {
                    drivers
                        .entry(key)
                        .or_default()
                        .push((pins.pin(e.port, e.port), None));
                } else {
                    // Fold a `~operand` inverter: load under the operand signal (the
                    // dropped Not's input), on the pin `make_gate_box` keyed the same
                    // way — so the wire runs operand -> bubbled pin, no Not box.
                    let (endpoint, _) = fold_inverter(design, e.endpoint);
                    let key = anchors.boundary_pin(endpoint).unwrap_or(endpoint);
                    loads
                        .entry(key)
                        .or_default()
                        .push((pins.pin(e.port, endpoint), e.select.clone()));
                }
                continue;
            }
            // Plain instance ports fold in too (#116): an output drives the
            // scope signal, an input loads it — otherwise a net driven only by
            // an instance (e.g. `core_trap` from the core's `trap` port) has
            // loads but no driver and its wire to an FF/Comb is dropped. The
            // structural pass already draws instance<->instance connections,
            // so the shared `seen` set below dedups those pairs. Interface
            // machinery keeps its own folds: only Instance parents qualify
            // (an instance inside a dissolved GenBlock is itself hoisted into
            // `box_set` by `child_boxes`, so its ports fold like any other).
            let Some(pn) = design.node(e.port) else {
                continue;
            };
            if pn.kind != NodeKind::Port {
                continue;
            }
            let Some(parent) = pn.parent else { continue };
            if !anchors.box_set.contains(&parent) || !is_kind(design, parent, NodeKind::Instance) {
                continue;
            }
            let anchor = (e.port, e.select.clone());
            match e.dir {
                Dir::Out => drivers.entry(key).or_default().push(anchor),
                Dir::In => loads.entry(key).or_default().push(anchor),
                _ => {
                    drivers.entry(key).or_default().push(anchor.clone());
                    loads.entry(key).or_default().push(anchor);
                }
            }
        }
        // Boundary-like anchors fold in by declared direction: an input drives
        // its signal inside the scope, an output loads it, an inout does both.
        // Shared by the scope's own ports and by modport member pins keyed on
        // the underlying member node — the same key the logic edges above use
        // for bundle signals — so the two stay symmetric by construction.
        let mut fold_anchor = |key: NodeId, pin: NodeId| match design.node(pin).and_then(|n| n.dir)
        {
            Some(Dir::Out) => loads.entry(key).or_default().push((pin, None)),
            Some(Dir::In) => drivers.entry(key).or_default().push((pin, None)),
            _ => {
                drivers.entry(key).or_default().push((pin, None));
                loads.entry(key).or_default().push((pin, None));
            }
        };
        for &p in &anchors.own_ports {
            fold_anchor(p, p);
        }
        for (&sig, pins_of_sig) in &anchors.iface_pin {
            for &(_, pin) in pins_of_sig {
                fold_anchor(sig, pin);
            }
        }
        // Bundle members fold under their bundle's raw access port (#96): an
        // in-scope logic box reading or driving `bus.valid` wires to the
        // aggregate port. The synthetic pin has no model node, so it folds as
        // both a driver and a load — the aggregate carries traffic both ways.
        for (&member, &owner) in &anchors.iface_owner {
            if member == owner {
                continue;
            }
            if let Some(&rp) = anchors.raw_port.get(&owner) {
                fold_anchor(member, rp);
            }
        }
        // Every gate box offers its result under its own id key (#157): a non-root
        // gate feeding another gate has no `out` edge, so its output pin is offered
        // here, and the consumer's `in` edge (loaded under the producer's id above)
        // meets it. A root gate's self-key has no loads and is simply skipped.
        for &b in &anchors.boxes {
            if is_gate(design, b) {
                drivers.entry(b).or_default().push((pins.pin(b, b), None));
            }
        }
        // A Memory box drives its own array node (#206): a gate/mux reading the whole
        // array (`cpuregs[decoded_rs1]`) loads key = the memory. Unlike a `Dout` edge
        // (which drives a specific read target), that read has no output pin, so give
        // the box a read-out pin here and offer it as the driver — the wire then reaches
        // the memory glyph. Added only when something loads the array, so a memory with
        // no whole-array gate read is unchanged.
        for &b in &anchors.boxes {
            if is_kind(design, b, NodeKind::Memory) && loads.contains_key(&b) {
                let pid = pins.pin(b, b);
                if let Some(node) = nodes.iter_mut().find(|n| n.id == b) {
                    if !node.ports.iter().any(|p| p.id == pid) {
                        node.ports.push(SchPort {
                            more: None,
                            id: pid,
                            name: design.node(b).map(|n| n.name.clone()).unwrap_or_default(),
                            side: Side::East,
                            path: design.node(b).map(|n| n.path.clone()).unwrap_or_default(),
                            width: None,
                            role: Some(PinRole::Dout),
                            bundle: false,
                            dangling: false,
                            constant: None,
                        });
                    }
                }
                drivers.entry(b).or_default().push((pid, None));
            }
        }
        // BTreeSet for a deterministic wire order across runs.
        let signals: std::collections::BTreeSet<NodeId> =
            drivers.keys().chain(loads.keys()).copied().collect();
        for sig in signals {
            let (Some(ds), Some(ls)) = (drivers.get(&sig), loads.get(&sig)) else {
                continue;
            };
            join_signal(
                design.node(sig),
                scope_path,
                ds,
                ls,
                &mut seen,
                &mut next_edge,
                &mut edges,
            );
        }
    }

    // Constant tie-offs: a small source node outside each box, wired to every
    // input the model records as driven by a literal. ELK lays these out left of
    // the consumer and routes other wires around them.
    for &b in &anchors.boxes {
        let Some(bn) = design.node(b) else { continue };
        for &pid in &bn.children {
            if !is_kind(design, pid, NodeKind::Port) {
                continue;
            }
            if let Some(lit) = design.node(pid).and_then(|n| n.const_value.clone()) {
                nodes.push(make_const_node(&lit, pid));
                edges.push(SchEdge {
                    id: next_edge,
                    source: CONST_ID_BASE + pid,
                    target: pid,
                    net: None,
                    net_path: None,
                });
                next_edge += 1;
            }
        }
    }

    // Gate/mux/datapath operand constants (#199) render inline beside the pin (the
    // value is carried on `SchPort.constant` by `make_gate_box`), so — unlike the
    // instance-port tie-off above — no separate source node is synthesized.

    // Mark dangling output pins on synthesized logic boxes (#118) — an FF/comb/
    // assign output that nothing in this scope reads stays visible (dimmed by
    // the frontend) instead of being pruned, so a write-only register still
    // shows its Q with the signal's name.
    let wired: std::collections::HashSet<NodeId> =
        edges.iter().flat_map(|e| [e.source, e.target]).collect();
    for node in &mut nodes {
        let gate = is_gate(design, node.id);
        if !(is_logic_box(design, node.id) || gate) {
            continue;
        }
        // A gate's result pin is unnamed (keyed on the gate itself, so a wired output
        // is labelled by its net wire). When the driven net has no in-scope reader the
        // pin dangles with no wire to name it, so relabel it with that net (#202) — the
        // floating wire then stays visible and cross-probeable, mirroring a dangling
        // FF Q's in-box net label (#118). A logic box's output pin already carries its
        // signal name/path, and a nested gate has no `out` edge, so both are left as-is.
        let driven_net = gate
            .then(|| {
                design
                    .edges_of(node.id)
                    .into_iter()
                    .find(|e| e.port == node.id && e.dir == Dir::Out)
                    .and_then(|e| design.node(e.endpoint))
            })
            .flatten();
        for p in &mut node.ports {
            if p.side == Side::East && !wired.contains(&p.id) {
                p.dangling = true;
                if let Some(net) = driven_net {
                    p.name = net.name.clone();
                    p.path = net.path.clone();
                }
            }
        }
    }

    Some(SchematicGraph {
        truncated: false,
        root: scope_path.to_string(),
        nodes,
        edges,
    })
}

/// Expand an instance at the process level: the scope graph one level down inside
/// it. Delegates to [`expand_with`] with [`Projection::ProcessLevel`].
pub fn expand(design: &Design, instance: NodeId) -> Option<SchematicGraph> {
    expand_with(design, instance, Projection::ProcessLevel)
}

/// Expand an instance under a chosen [`Projection`] (#157) — the scope graph one
/// level down inside it, at the process or gate level.
pub fn expand_with(
    design: &Design,
    instance: NodeId,
    projection: Projection,
) -> Option<SchematicGraph> {
    let path = design.node(instance)?.path.clone();
    scope_graph_with(design, &path, projection)
}

/// Cross one signal's drivers with its loads into wires.
///
/// The shared join used by the scope graph's signal pass and, from #244, by the
/// cone extractor — so the two cannot drift on what counts as a connection. Each
/// pin pair is deduped against `seen` on `(min, max)`, which is what collapses a
/// high-fan-out net instead of emitting one wire per load.
///
/// `sig` is the signal node itself (`None` when it is not in the model), used
/// only for the wire's label and `net_path`; `scope_path` is what the label is
/// made relative to.
fn join_signal(
    sig: Option<&Node>,
    scope_path: &str,
    drivers: &[(NodeId, Option<String>)],
    loads: &[(NodeId, Option<String>)],
    seen: &mut std::collections::HashSet<(NodeId, NodeId)>,
    next_edge: &mut u32,
    out: &mut Vec<SchEdge>,
) {
    let label = sig.map(|n| relative_to(&n.path, scope_path));
    let net_path = sig.map(|n| n.path.clone());
    for &(dpin, ref dsel) in drivers {
        for &(lpin, ref lsel) in loads {
            // No self-loop (a box reading and writing the same signal); a
            // driver and a load touching *different* bits of a vector are
            // not connected (#116: core 0 drives core_trap[0], lane 1's FF
            // reads [1]); dedup against the shared `seen` set last so a
            // skipped pair doesn't block a legitimate one.
            if dpin == lpin
                || matches!((dsel, lsel), (Some(a), Some(b)) if a != b)
                || !seen.insert((dpin.min(lpin), dpin.max(lpin)))
            {
                continue;
            }
            let select = dsel.clone().or_else(|| lsel.clone());
            let net = label.clone().map(|b| with_select(b, &select));
            out.push(SchEdge {
                id: *next_edge,
                source: dpin,
                target: lpin,
                net,
                net_path: net_path.clone(),
            });
            *next_edge += 1;
        }
    }
}

/// Traversal budget for [`cone_with`] (#244).
///
/// Every cap is a *rendering* policy: what it drops is reported back through
/// [`SchPort::more`] and [`SchematicGraph::truncated`], never silently
/// discarded. Defaults come from the `nav` benchmark rather than from feel —
/// see each field.
///
/// Deserialized field-by-field (#244 PR2) so the `trace_graph` command can take
/// a partial `{"fanout": 8}` and inherit the rest — a caller that names one cap
/// should not have to restate the two it does not care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConeLimits {
    /// Maximum hops from the seed. One hop is signal → box → that box's other
    /// signals.
    #[serde(default = "default_depth")]
    pub depth: usize,
    /// Maximum boxes reached through any one signal, per hop. This is the cap
    /// that tames a global clock/reset: fan-out, not node count, is what makes
    /// a cone expensive (ADR 0003).
    #[serde(default = "default_fanout")]
    pub fanout: usize,
    /// Maximum boxes in the whole returned graph — the only cap that bounds
    /// `fanout^depth`.
    #[serde(default = "default_boxes")]
    pub boxes: usize,
}

// Serde needs a fn per field; each defers to `Default` so there is exactly one
// place the numbers (and the reasoning below) live.
fn default_depth() -> usize {
    ConeLimits::default().depth
}
fn default_fanout() -> usize {
    ConeLimits::default().fanout
}
fn default_boxes() -> usize {
    ConeLimits::default().boxes
}

impl Default for ConeLimits {
    fn default() -> Self {
        Self {
            // A real debug chain is signal → driving process → its register →
            // that register's clock/enable source. `nav` uses 6; depth alone
            // cannot bound the walk (4 hops at fanout 32 is ~1.0M boxes),
            // which is why `boxes` exists.
            depth: 4,
            // Above every process-level box fan-out in the committed golden —
            // its hottest net (`g_lane[0].core.resetn`) has degree 110 but only
            // 11 box loads — so no real fixture net truncates at the default,
            // while the 1M synthetic's 59,049-load clock caps at the first hop.
            fanout: 32,
            // ~16 ms (one frame) at the 31 us/box measured on the `golden`
            // basis of the `nav` scenario (227 boxes in 7.11 ms, 2026-08-01).
            // An earlier 2000 was extrapolated from the legacy cone's 2.4-3.2
            // us/box and was ~4x too generous: this walk synthesizes pins and
            // resolves a box per endpoint, so it costs far more per box than
            // the instance-only one it replaces. Measured on golden rather
            // than a synthetic basis deliberately — docs/benchmarking.md
            // warns that synthetic absolute latencies are not user-facing.
            boxes: 500,
        }
    }
}

impl ConeLimits {
    /// Default caps with an explicit hop count.
    pub fn depth(depth: usize) -> Self {
        Self {
            depth,
            ..Self::default()
        }
    }
}

/// One expansion request in a trace (#244 PR2): walk `depth` hops out of `seed`
/// in `dir`.
///
/// A trace is an *ordered list* of these, and [`trace_graph`] re-walks the whole
/// list on every expansion rather than merging a new hop into an existing graph.
/// That is not a convenience: [`PinAlloc`] hands out pin ids from a per-call
/// counter, so the same `(box, signal)` gets a different id in a second call and
/// the two graphs' pins collide misaligned with meaning — see [`cone_with`]'s
/// note. Re-deriving keeps every id inside one call, which is the only place
/// they are comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceStep {
    /// A net, port, or logic/instance node to expand from.
    pub seed: NodeId,
    /// Which way to follow connectivity: fan-in, fan-out, or both.
    pub dir: Dir,
    /// Hops for this step alone. One click is normally one hop; the caller is
    /// expected to clamp this to [`ConeLimits::depth`].
    pub depth: usize,
    /// Overrides [`ConeLimits::fanout`] for this step only (#244 PR4).
    ///
    /// Backs the "N more…" affordance: a pin reports its dropped connections on
    /// [`SchPort::more`], and expanding *that* signal must reveal them without
    /// un-capping every other signal in the trace — which is what raising the
    /// shared budget would do, dragging a global clock's whole fan-out onto a
    /// canvas the user never asked about.
    ///
    /// `None` inherits the shared budget. A large value is safe rather than
    /// unbounded: [`ConeLimits::boxes`] still bounds the graph, so "expand this
    /// signal fully" degrades to "as far as the box budget allows, and still
    /// reports the remainder" instead of blowing up on a 59K-load net.
    pub fanout: Option<usize>,
}

/// Whether `e` leaves `sig` in the requested direction.
///
/// `Edge.dir` is relative to `e.port`, not to whichever end the walk is
/// standing on — the signal join reads `is_logic_box(e.port) && e.dir ==
/// Dir::Out` as *"this box drives `e.endpoint`"*. So the test has to flip
/// depending on which side `sig` sits on. The legacy [`cone`] omits that flip,
/// which is why it reports a net's fan-in when asked for fan-out.
fn follows(e: &Edge, sig: NodeId, dir: Dir) -> bool {
    let near_is_port = e.port == sig;
    match dir {
        // Fan-out: something reads the signal.
        Dir::Out => {
            if near_is_port {
                e.dir == Dir::Out
            } else {
                e.dir != Dir::Out
            }
        }
        // Fan-in: something drives it.
        Dir::In => {
            if near_is_port {
                e.dir != Dir::Out
            } else {
                e.dir == Dir::Out
            }
        }
        Dir::Inout => true,
    }
}

/// Whether the box end of `e` drives `sig` (rather than reading it).
fn edge_drives(e: &Edge, sig: NodeId) -> bool {
    if e.port == sig {
        e.dir != Dir::Out
    } else {
        e.dir == Dir::Out
    }
}

/// The signals a cone starts from.
///
/// A box seed starts at the signals it touches. A signal seed folds in its
/// same-path siblings, because a port and its backing net are two nodes for one
/// wire (the golden's `bus.valid` is a `Var` *and* a `Port`) and a seed on
/// either must reach both.
fn seed_signals(design: &Design, seed: NodeId, dir: Dir) -> Vec<NodeId> {
    let Some(n) = design.node(seed) else {
        return Vec::new();
    };
    if is_logic_box(design, seed)
        || is_gate(design, seed)
        || is_kind(design, seed, NodeKind::Memory)
    {
        // Reached only when the seed is *not* a box in this projection — a gate
        // at process level is interior to its block, so it has no pins of its own
        // to anchor on and its neighbours are the best frontier available.
        // ([`trace_graph`] walks a seed that *is* a box as itself.)
        //
        // Even then, only the neighbours on the side asked for. Folding in both
        // sides made a directional expansion walk the wrong one as well: fan-out
        // from a gate followed its operands and drew their neighbourhood (#286).
        // `Edge.dir` is relative to `e.port`, so which end the seed stands on
        // decides the test; `Inout` keeps both.
        let drives = |e: &Edge| {
            if e.port == seed {
                e.dir == Dir::Out
            } else {
                e.dir != Dir::Out
            }
        };
        let mut out: Vec<NodeId> = design
            .edges_of(seed)
            .iter()
            .filter(|e| match dir {
                Dir::Out => drives(e),
                Dir::In => !drives(e),
                Dir::Inout => true,
            })
            .map(|e| if e.port == seed { e.endpoint } else { e.port })
            .collect();
        out.sort_unstable();
        out.dedup();
        return out;
    }
    let mut out = design.nodes_at_path(&n.path).to_vec();
    if out.is_empty() {
        out.push(seed);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The instance that contains `node` on canvas, or `None` for one directly under
/// the design top (#293).
///
/// The nearest ancestor `Instance`, with generate blocks dissolving exactly as
/// [`child_boxes`] dissolves them — so `g_lane[0].core` contains its own logic
/// while `g_lane[0]` itself is not a container. The design top is excluded
/// deliberately: it contains everything, so drawing it would wrap every trace in
/// one outer box that says nothing.
fn containing_instance(design: &Design, node: NodeId) -> Option<NodeId> {
    let mut cur = design.node(node)?.parent?;
    loop {
        let n = design.node(cur)?;
        if n.kind == NodeKind::Instance {
            // A top-level instance is the design itself, not a container.
            return n.parent.map(|_| cur);
        }
        cur = n.parent?;
    }
}

/// The same-path sibling group of a signal — a port and its backing net read as
/// the **one** signal they are (#285).
///
/// A module port is two model nodes at one canonical path with *no edge between
/// them*: the `Port` carries only the external connection, the backing
/// `Net`/`Var` every internal one. [`seed_signals`] already folds a *seed* that
/// way; this is the same fold applied at every hop, so a port reached mid-walk
/// unifies with its backing net exactly as a seeded one does. Walking the two
/// halves separately gave each its own join and its own anchor, which drew the
/// port twice — once on each side of the wall, with nothing between them.
///
/// A node that is not itself a signal groups alone: a `Memory` array is both a
/// signal and a box, and folding a box into a group would let the group's
/// representative name a node some builder already emitted pins for.
fn signal_group(design: &Design, sig: NodeId) -> Vec<NodeId> {
    let is_signal = |id: NodeId| {
        matches!(
            design.node(id).map(|n| n.kind),
            Some(NodeKind::Port | NodeKind::Net | NodeKind::Var)
        )
    };
    let Some(n) = design.node(sig).filter(|_| is_signal(sig)) else {
        return vec![sig];
    };
    let mut out: Vec<NodeId> = design
        .nodes_at_path(&n.path)
        .iter()
        .copied()
        .filter(|&m| is_signal(m))
        .collect();
    if out.is_empty() {
        out.push(sig);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// [`signal_group`], memoized on every member.
///
/// Keyed on each member rather than on the query, so the second half of a dual
/// node is free — and so the wall-crossing arm, which asks per *candidate edge*
/// rather than per signal, stays O(distinct signals touched). Handed out as an
/// `Rc` because that arm is the hot one on a high-fan-out net: a clone must be a
/// refcount bump, not a fresh allocation per candidate edge.
fn group_of(
    cache: &mut std::collections::HashMap<NodeId, std::rc::Rc<[NodeId]>>,
    design: &Design,
    sig: NodeId,
) -> std::rc::Rc<[NodeId]> {
    if let Some(g) = cache.get(&sig) {
        return g.clone();
    }
    let g: std::rc::Rc<[NodeId]> = signal_group(design, sig).into();
    for &m in g.iter() {
        cache.insert(m, g.clone());
    }
    g
}

/// The box a reached node belongs to, plus the scope that box lives in.
///
/// The generalization of `nearest_instance` that every box kind needs: walk up
/// until an ancestor is one of its own parent's [`child_boxes`]. Because the
/// predicate *is* the scope graph's, `GenBlock` dissolution, [`Projection`] and
/// every box kind fall out for free — the two views cannot disagree on what a
/// box is. `None` for a node under no box (a scope-level net, or a module port).
fn cone_box_of(
    design: &Design,
    node: NodeId,
    projection: Projection,
    cache: &mut ScopeCache,
) -> Option<(NodeId, NodeId)> {
    // Under GateLevel a primitive can be a box, but it is a flat child of its
    // process block rather than of the scope `child_boxes` dissolves — so the
    // walk below would step straight past it and land on the enclosing block.
    //
    // "Can be", not "is": `child_boxes` dissolves only `Comb`/`Latch`/`Assign`
    // (through any `GenBlock`), and `gate_children` drops a foldable inverter.
    // Ask it rather than assume, or the cone draws boxes the scope graph does
    // not — a phantom inverter, or the internal mux cloud of an `Ff`, which
    // stays opaque at both projections (#268).
    if projection == Projection::GateLevel && is_gate(design, node) {
        // A single-fanout `~operand` is drawn as an `Inv` bubble on its
        // consumer's input pin, never as a box. Redirect to that consumer (the
        // one reader `is_foldable_not` guarantees) so the wire still lands.
        let bx = if is_foldable_not(design, node) {
            not_reader(design, node)?
        } else {
            node
        };
        // The scope a dissolved gate would belong to: past its own block and
        // any generate wrappers, both of which `child_boxes` sees through.
        let mut scope = design.node(bx).and_then(|n| n.parent);
        while matches!(
            scope.and_then(|s| design.node(s)).map(|n| n.kind),
            Some(NodeKind::Comb | NodeKind::Latch | NodeKind::Assign | NodeKind::GenBlock)
        ) {
            scope = scope.and_then(|s| design.node(s)).and_then(|n| n.parent);
        }
        if let Some(s) = scope {
            if cache.box_set(design, s, projection).contains(&bx) {
                return Some((bx, s));
            }
        }
        // Not dissolved anywhere — the gate sits inside an opaque box. Fall
        // through to the ordinary walk, which lands on that box.
    }
    let mut cur = node;
    loop {
        let parent = design.node(cur)?.parent?;
        if cache.box_set(design, parent, projection).contains(&cur) {
            return Some((cur, parent));
        }
        cur = parent;
    }
}

/// Per-scope box membership and [`ScopeAnchors`], each built on first touch.
///
/// A cone walks an arbitrary node set across many scopes, so it cannot hold one
/// set of anchors the way [`scope_graph_with`] does — but every scope it touches
/// gets the *same* anchors that view would build for it, which is the whole
/// point: the two cannot then disagree about which pin a wire lands on.
///
/// The two halves are cached apart because they are wanted in very different
/// volumes. [`cone_box_of`]'s walk-up tests membership at *every* ancestor —
/// blocks, generate wrappers, instances — while only a box that actually
/// anchors a wire needs the full anchoring. Building all of it per ancestor
/// costs a `boundary_of` pass that is quadratic in a scope's ports x children,
/// which measurably slowed the `nav` benchmark's cone for nothing.
#[derive(Default)]
struct ScopeCache {
    boxes: std::collections::HashMap<NodeId, std::collections::HashSet<NodeId>>,
    anchors: std::collections::HashMap<NodeId, ScopeAnchors>,
}

impl ScopeCache {
    /// Box membership alone — the cheap half, and all the walk-up needs.
    fn box_set(
        &mut self,
        design: &Design,
        scope: NodeId,
        projection: Projection,
    ) -> &std::collections::HashSet<NodeId> {
        self.boxes
            .entry(scope)
            .or_insert_with(|| child_boxes(design, scope, projection).into_iter().collect())
    }

    /// The full anchoring, built only where a pin must actually be resolved.
    fn anchors(&mut self, design: &Design, scope: NodeId, projection: Projection) -> &ScopeAnchors {
        self.anchors
            .entry(scope)
            .or_insert_with(|| ScopeAnchors::for_scope(design, scope, projection))
    }
}

/// Build the `SchNode` for a reached box, dispatching to the same `make_*`
/// builders the scope graph uses so pin keying is identical.
fn cone_box_node(
    design: &Design,
    bx: NodeId,
    scope_path: &str,
    pins: &mut PinAlloc,
) -> Option<SchNode> {
    let kind = design.node(bx)?.kind;
    match kind {
        NodeKind::Ff => make_ff_box(design, bx, scope_path, pins),
        NodeKind::Comb | NodeKind::Latch | NodeKind::Assign => make_logic_box(design, bx, pins),
        NodeKind::Memory => make_memory_box(design, bx, scope_path, pins),
        k if is_gate_kind(k) => make_gate_box(design, bx, pins),
        _ => make_box(design, bx, scope_path),
    }
}

/// Whether `id` is `bx` or lives somewhere under it.
fn within_box(design: &Design, id: NodeId, bx: NodeId) -> bool {
    let mut cur = id;
    loop {
        if cur == bx {
            return true;
        }
        match design.node(cur).and_then(|n| n.parent) {
            Some(p) => cur = p,
            None => return false,
        }
    }
}

/// The pin on `bx` that `e` lands on, keyed exactly as that box's builder keyed
/// it — so the id is guaranteed to exist in the emitted node.
///
/// `inner` is the edge end that [`cone_box_of`] resolved to `bx`, i.e. the node
/// actually inside the box; `scope` is the scope that box lives in. Only the
/// instance/interface arm needs the scope's full anchoring, so the cache is
/// passed rather than the anchors — the other arms must not pay to build them.
#[allow(clippy::too_many_arguments)]
fn cone_pin_for(
    design: &Design,
    cache: &mut ScopeCache,
    scope: NodeId,
    projection: Projection,
    bx: NodeId,
    inner: NodeId,
    e: &Edge,
    pins: &mut PinAlloc,
) -> NodeId {
    let kind = design.node(bx).map(|n| n.kind);
    match kind {
        // `make_box` keys these on real model ids — a child `Port`, a `Modport`
        // view, or the synthesized `RAW_PORT_BASE + bx` raw access port — so the
        // scope graph's own rule is the only thing that names them correctly
        // (#269: a bare bundle's raw member is a `Var`, and guessing "a Port
        // child of the box" left it anchored on an id `make_box` never emitted).
        Some(NodeKind::Instance) | Some(NodeKind::Interface) => {
            // `inner` first — it is the end `cone_box_of` placed in this box —
            // then the edge's own ends, because the port/backing-net dual node
            // means the pin-bearing `Port` is often the *other* end of the same
            // edge. Each candidate must actually live in `bx`: `pin_in_box`'s
            // bundle rules are keyed on the scope, so a member of a *different*
            // bundle would otherwise resolve to that bundle's raw port.
            // ...and, last, `inner`'s same-path siblings. A module port is two
            // nodes and only the `Port` half bears the box's pin (#285); when the
            // walk arrives from a *gate* inside the instance, neither end of the
            // edge is that half — the edge runs gate-to-backing-`Var` — so the
            // pin resolved to nothing and the connection was dropped by the
            // `anchored` test below (#286).
            let mut cands: Vec<NodeId> = vec![inner, e.port, e.endpoint];
            if let Some(n) = design.node(inner) {
                cands.extend(design.nodes_at_path(&n.path).iter().copied());
            }
            let anchors = cache.anchors(design, scope, projection);
            for cand in cands {
                if !within_box(design, cand, bx) {
                    continue;
                }
                let pin = anchors.pin_in_box(design, cand, bx);
                if pin != bx {
                    return pin;
                }
            }
            bx
        }
        Some(k) if is_gate_kind(k) => {
            if e.port == bx && e.dir == Dir::Out {
                pins.pin(bx, bx)
            } else {
                pins.pin(bx, fold_inverter(design, e.endpoint).0)
            }
        }
        _ => pins.pin(bx, e.endpoint),
    }
}

/// A one-pin node standing in for a signal the trace reaches but that no box on
/// canvas owns — the seed itself, or a module port where the walk crosses a
/// hierarchy wall. Keyed on the model id like [`make_boundary_pin`], so the pin
/// cross-probes the real signal.
fn make_signal_stub(design: &Design, sig: NodeId, side: Side) -> Option<SchNode> {
    let n = design.node(sig)?;
    Some(SchNode {
        parent: None,
        id: sig,
        kind: n.kind,
        label: n.name.clone(),
        path: n.path.clone(),
        expandable: false,
        ports: vec![SchPort {
            id: sig,
            name: n.name.clone(),
            side,
            path: n.path.clone(),
            width: pin_width(design, &n.type_),
            role: None,
            bundle: false,
            dangling: false,
            constant: None,
            more: None,
        }],
        module: None,
        constant: None,
        modport: None,
        mem_depth: None,
        init_source: None,
    })
}

/// Stand up a one-pin anchor for a signal no box on canvas owns, and return the
/// pin to wire to (#286).
///
/// Keyed on the signal's *group* representative, not on the node the edge
/// happened to name, so the next hop finds the anchor already there instead of
/// standing up a second node for the same signal (#285).
///
/// `None` when nothing may stand here: a node the scope graph draws inline (a
/// constant tie, a folded inverter) or a gate primitive. An anchor represents a
/// *signal* the trace reaches, and at process level a gate is interior to its
/// block — stubbing one drew a `Mux` the scope graph never shows (#268).
#[allow(clippy::too_many_arguments)]
fn free_net_anchor(
    design: &Design,
    projection: Projection,
    node: NodeId,
    side: Side,
    group_cache: &mut std::collections::HashMap<NodeId, std::rc::Rc<[NodeId]>>,
    stubs: &mut std::collections::HashSet<NodeId>,
    emitted: &std::collections::HashSet<NodeId>,
    emitted_pins: &mut std::collections::HashSet<NodeId>,
    nodes: &mut Vec<SchNode>,
) -> Option<NodeId> {
    // Only a signal, and by kind rather than by "whatever had no box". A block
    // the scope graph dissolves — an `Assign` at gate level — reaches here too,
    // and standing it up drew a box that view never shows, which
    // `cone_with_agrees_with_the_scope_graph_on_what_is_a_box` caught.
    if !matches!(
        design.node(node).map(|n| n.kind),
        Some(NodeKind::Port | NodeKind::Net | NodeKind::Var)
    ) {
        return None;
    }
    let far = group_of(group_cache, design, node)[0];
    if !stubs.contains(&far)
        && !emitted.contains(&far)
        && !is_drawn_inline(design, far, projection)
        && !is_gate(design, far)
    {
        if let Some(n) = make_signal_stub(design, far, side) {
            emitted_pins.extend(n.ports.iter().map(|p| p.id));
            nodes.push(n);
            stubs.insert(far);
        }
    }
    stubs.contains(&far).then_some(far)
}

/// The scope a cone's breadcrumb binds to: the nearest enclosing navigable
/// scope, so `root` is a path `scope_graph` can actually open.
fn cone_root(design: &Design, seed: NodeId) -> String {
    let mut cur = seed;
    loop {
        if is_navigable_scope(design, cur) {
            if let Some(n) = design.node(cur) {
                return n.path.clone();
            }
        }
        match design.node(cur).and_then(|n| n.parent) {
            Some(p) => cur = p,
            None => {
                return design
                    .node(seed)
                    .map(|n| n.path.clone())
                    .unwrap_or_default()
            }
        }
    }
}

/// Fan-in/out cone of a `seed` (a net, port, or logic/instance node), built on
/// the scope graph's own box/pin machinery (#244).
///
/// Unlike the legacy [`cone`] this emits **every** box kind the scope graph
/// draws, gives both ends of every wire a real [`PinAlloc`] pin id so a layout
/// engine can anchor them, crosses hierarchy walls by path lookup, and enforces
/// `limits` *visibly* — a capped signal reports its remainder on
/// [`SchPort::more`] and sets [`SchematicGraph::truncated`].
///
/// It is deliberately **not** output-compatible with [`cone`]: besides the pin
/// ids, the direction filter is corrected (see [`follows`]). New callers want
/// this one; `cone` is retained as the `svxprobe graph --cone` contract and the
/// `scale-bench` fan-out baseline.
///
/// Pin ids are stable within one call, in allocation order — they are *not*
/// comparable across calls, so an incremental merge must re-key rather than
/// assume. [`trace_graph`] sidesteps that by re-deriving instead of merging.
pub fn cone_with(
    design: &Design,
    seed: NodeId,
    dir: Dir,
    limits: ConeLimits,
    projection: Projection,
) -> SchematicGraph {
    trace_graph(
        design,
        &[TraceStep {
            seed,
            dir,
            depth: limits.depth,
            fanout: None,
        }],
        limits,
        projection,
    )
}

/// The accumulated fan-in/out of an ordered list of [`TraceStep`]s — the trace
/// mode's extractor (#244 PR2), and the generalization [`cone_with`] delegates to.
///
/// Every step walks the same machinery a single-seed cone does; what makes them
/// *one* graph rather than several is that they share the walk's state. Nothing
/// merges anything:
///
/// * `emitted` / `emitted_pins` already dedupe boxes, and [`PinAlloc`] memoizes
///   on `(box, signal)`, so a box two steps both reach is emitted once with the
///   same pins.
/// * `seen_pairs` already dedupes wires on the `(min, max)` pin pair, and
///   `next_edge` keeps counting, so edge ids stay unique across steps.
/// * `visited` only gates what is pushed onto the *next* hop — a step's own
///   frontier is always walked, so re-seeding on a signal an earlier step
///   reached still expands it.
/// * `stubs` is the load-bearing one. [`follows`] filters to one direction, so a
///   directional walk is one-sided by construction and always synthesizes a
///   signal stub. Expanding fan-in of a net and then its fan-out therefore
///   re-uses that one stub — step 1 hangs it as a load, step 2 finds it already
///   there and hangs it as a driver — so the net converges on a single junction
///   node with its drivers on one side and its loads on the other, instead of
///   being drawn twice.
///
/// `limits.boxes` is the budget for the *whole* graph, so an accumulating trace
/// stays bounded no matter how many steps the user adds. `root` binds to the
/// first step's seed; an empty `steps` yields an empty graph.
pub fn trace_graph(
    design: &Design,
    steps: &[TraceStep],
    limits: ConeLimits,
    projection: Projection,
) -> SchematicGraph {
    let root = steps
        .first()
        .map(|s| cone_root(design, s.seed))
        .unwrap_or_default();
    let mut pins = PinAlloc::new();
    let mut nodes: Vec<SchNode> = Vec::new();
    let mut emitted: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    // Anchor stubs are tracked apart from boxes: their pin id is the model id,
    // which is only valid for a node this function synthesized.
    let mut stubs: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    let mut emitted_pins: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    let mut edges: Vec<SchEdge> = Vec::new();
    let mut seen_pairs: std::collections::HashSet<(NodeId, NodeId)> =
        std::collections::HashSet::new();
    let mut next_edge = design.edges().len() as u32;
    let mut truncated = false;
    let mut scopes = ScopeCache::default();
    let mut more_on: std::collections::HashMap<NodeId, u32> = std::collections::HashMap::new();

    let mut visited: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    let mut group_cache: std::collections::HashMap<NodeId, std::rc::Rc<[NodeId]>> =
        std::collections::HashMap::new();
    // The anchor of every step's own seed. A walk that reaches nothing must still
    // draw the signal the user pointed at: an empty graph reads as "no such
    // signal" and is indistinguishable from a failed lookup — the silent discard
    // ADR 0003 forbids, and what a fan-in on any undriven input returned (#285).
    let mut seed_anchors: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    // Seeds that are themselves boxes, and the scope each lives in (#286).
    let mut box_seeds: std::collections::HashMap<NodeId, NodeId> = std::collections::HashMap::new();

    for step in steps {
        // Is the seed a *box* rather than a signal? Asked of `cone_box_of`, so
        // the answer is the scope graph's own — a gate is a box at gate level
        // and interior to its block at process level, and this follows without
        // a second opinion.
        //
        // A box seed is walked as itself, never expanded into its neighbours.
        // The model wires a gate **directly** to the next gate, with no net node
        // between, so a neighbour *is* the box on the far side: seeding there
        // would show what *that* box drives, one level past what was asked for.
        // Standing on the box instead lets `follows` apply the direction, which
        // is the filter it exists for.
        let seed_box = cone_box_of(design, step.seed, projection, &mut scopes)
            .filter(|&(bx, _)| bx == step.seed);
        // Per step, not per call: a later step re-seeds a fresh frontier, and
        // `visited` carries over so the walk does not re-expand ground an
        // earlier step already covered. A seed that is itself already visited
        // is still walked — `visited` gates only what the *next* hop picks up.
        let mut frontier = match seed_box {
            Some((bx, scope)) => {
                box_seeds.insert(bx, scope);
                // Emitted up front, because a box seed anchors its wires on its
                // *own* pins — one per operand — rather than on the single stub a
                // signal converges to. Without it the join has a load and no
                // driver, draws nothing, and the orphan pass then removes the box
                // the far side reached, so a trace seeded on a gate came back
                // empty. It also gives a box seed the guarantee a signal seed
                // already had: a trace always draws the thing it was seeded on.
                if !emitted.contains(&bx) && !stubs.contains(&bx) {
                    let scope_path = design
                        .node(scope)
                        .map(|n| n.path.clone())
                        .unwrap_or_else(|| root.clone());
                    if let Some(node) = cone_box_node(design, bx, &scope_path, &mut pins) {
                        emitted_pins.extend(node.ports.iter().map(|p| p.id));
                        nodes.push(node);
                        emitted.insert(bx);
                    }
                }
                seed_anchors.insert(bx);
                vec![bx]
            }
            None => seed_signals(design, step.seed, step.dir),
        };
        visited.extend(frontier.iter().copied());
        for &s in &frontier {
            seed_anchors.insert(group_of(&mut group_cache, design, s)[0]);
        }

        for _hop in 0..step.depth.max(1) {
            let mut next: Vec<NodeId> = Vec::new();
            // Two frontier entries at one path are one signal, so the group is
            // walked once. Per hop rather than per call, because the rule above —
            // a step's own frontier is always walked — must survive a re-seed on
            // ground an earlier step covered.
            let mut done: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
            for &sig in &frontier {
                let members = group_of(&mut group_cache, design, sig);
                let rep = members[0];
                if !done.insert(rep) {
                    continue;
                }
                // A sibling folded into this group counts as walked.
                visited.extend(members.iter().copied());
                // A group is one or two nodes, so a linear scan beats hashing —
                // and this is asked per candidate edge on a high-fan-out net.
                let in_group = |id: NodeId| members.contains(&id);
                let crossing = members.len() > 1;
                let is_seed = seed_anchors.contains(&rep);
                // The scope of a box seed, when this frontier entry is one: its
                // own pins are the anchors, so each edge is joined pin-to-pin
                // rather than through a shared stub.
                let seed_scope = box_seeds.get(&rep).copied();

                let mut drivers: Vec<(NodeId, Option<String>)> = Vec::new();
                let mut loads: Vec<(NodeId, Option<String>)> = Vec::new();
                let mut sig_pins: Vec<NodeId> = Vec::new();

                // Ascending model-edge order across the *whole* group, so the kept
                // set — and therefore pin allocation and the wire sequence — is
                // byte-stable across runs, and a port's two halves are budgeted as
                // the one signal they are rather than getting a cap each.
                let mut idx: Vec<u32> = members
                    .iter()
                    .flat_map(|&m| design.edge_indices_of(m).iter().copied())
                    .collect();
                idx.sort_unstable();
                idx.dedup();
                // `near` is the group member this edge actually touches. Both
                // `follows` and `edge_drives` read `Edge.dir` relative to `e.port`,
                // so they must be asked about the member the walk stands on — never
                // about the representative, which may be the other half.
                //
                // An edge *between* two group members is kept, not skipped: an
                // ordinary port and its backing net have none, but a
                // modport-specialized port shares its canonical path with the
                // member it views and is genuinely wired to it, and that edge is
                // how the bare bundle is reached (#269).
                //
                // Edges the direction *rejects* are carried along too, flagged.
                // Direction decides what a step newly **discovers**; it must not
                // decide what the seed is **connected** to. Expanding fan-in from
                // a pin on a box that reads the signal otherwise draws the driver
                // and leaves it dangling at the boundary, disconnected from the
                // very pin that was clicked — the answer detached from the
                // question (#286). Such an edge is wired only to something already
                // on canvas, so it adds no boxes and grows nothing.
                let cands: Vec<(NodeId, &Edge, bool)> = idx
                    .iter()
                    .map(|&i| &design.edges()[i as usize])
                    .map(|e| {
                        let near = if in_group(e.port) { e.port } else { e.endpoint };
                        (near, e, follows(e, near, step.dir))
                    })
                    .collect();
                let total = cands.iter().filter(|(_, _, f)| *f).count();
                let mut kept = 0usize;
                let mut dropped = 0u32;

                // `.max(1)` for the same reason as `depth` above: a zero budget would
                // drop every candidate on every signal, and the graph would come back
                // empty with `truncated` set but no pin anywhere to hang a `more`
                // count on — indistinguishable from "nothing found" in the UI, which
                // is precisely the silent discard this cap exists to prevent.
                for (near, e, followed) in cands {
                    if followed && kept >= step.fanout.unwrap_or(limits.fanout).max(1) {
                        dropped += 1;
                        continue;
                    }
                    let other = if e.port == near { e.endpoint } else { e.port };
                    // A connection the direction rejected: wire it only if the far
                    // side is already drawn. Never let it stand anything up, and
                    // never charge it against the fan-out budget — it is not a
                    // discovery, it is the seed meeting what is already there.
                    if !followed {
                        let drawn = match cone_box_of(design, other, projection, &mut scopes) {
                            Some((bx, _)) => emitted.contains(&bx),
                            None => stubs.contains(&group_of(&mut group_cache, design, other)[0]),
                        };
                        if !drawn {
                            continue;
                        }
                    }
                    // Which side the far end sits on, for an anchor's own pin.
                    let far_side = if edge_drives(e, near) {
                        Side::East
                    } else {
                        Side::West
                    };

                    // Resolve the far end to a pin to wire to: a box's own pin
                    // when that box actually exposes the signal, else a free-net
                    // anchor standing in for a signal no box on canvas owns.
                    let mut fresh: Option<SchNode> = None;
                    let mut far_box: Option<NodeId> = None;
                    let mut far_pin: Option<NodeId> = None;
                    let mut owned_by_a_box = false;
                    if let Some((bx, scope)) = cone_box_of(design, other, projection, &mut scopes) {
                        owned_by_a_box = true;
                        // `stubs` as well as `emitted`: a `SchNode.id` is a model
                        // id, and a node the walk already stood up as an anchor
                        // would otherwise be pushed a second time as a box, putting
                        // one id on two nodes — which no layout engine can resolve.
                        if !emitted.contains(&bx) && !stubs.contains(&bx) {
                            if nodes.len() >= limits.boxes {
                                truncated = true;
                                dropped += 1;
                                continue;
                            }
                            let scope_path = design
                                .node(scope)
                                .map(|n| n.path.clone())
                                .unwrap_or_else(|| root.clone());
                            let Some(node) = cone_box_node(design, bx, &scope_path, &mut pins)
                            else {
                                continue;
                            };
                            // Held back, not committed: the connection that reached
                            // this box may still fail to resolve to one of its pins,
                            // and a box pushed before that is known becomes an
                            // edge-less orphan on canvas (#269). `PinAlloc` memoizes
                            // per `(box, signal)`, so rebuilding the node on a later
                            // hop hands out the same ids — discarding one here costs
                            // nothing and shifts nothing.
                            fresh = Some(node);
                        }
                        let pin = cone_pin_for(
                            design,
                            &mut scopes,
                            scope,
                            projection,
                            bx,
                            other,
                            e,
                            &mut pins,
                        );
                        let anchored = emitted_pins.contains(&pin)
                            || fresh
                                .as_ref()
                                .is_some_and(|n| n.ports.iter().any(|p| p.id == pin));
                        if anchored {
                            far_box = Some(bx);
                            far_pin = Some(pin);
                        } else {
                            // The box does not expose this signal, so it cannot
                            // anchor the wire — but the signal is real and the walk
                            // must not lose it. This used to `continue`, which
                            // silently stranded any net *interior* to an instance
                            // the trace was already inside: `cone_box_of` answers
                            // "which box owns this node" with that instance, and a
                            // net it does not export has no pin there. Expanding a
                            // gate whose output feeds such a net therefore drew
                            // nothing at all. Fall through to a free-net anchor,
                            // which is what the signal actually is from in here.
                            fresh = None;
                        }
                    }
                    if far_pin.is_none() {
                        far_pin = free_net_anchor(
                            design,
                            projection,
                            other,
                            far_side,
                            &mut group_cache,
                            &mut stubs,
                            &emitted,
                            &mut emitted_pins,
                            &mut nodes,
                        );
                    }
                    let Some(pin) = far_pin else {
                        // Nothing may stand here — an inline tie, a gate primitive,
                        // or a block the scope graph dissolves. Keep walking through
                        // it only when *no* box owned it: that is the wall crossing.
                        // A signal a box owns but does not expose stays dropped, as
                        // it always has — the trace is outside that box and may not
                        // draw its interior, and enqueuing it walked the trace into
                        // boxes the scope graph does not draw.
                        if !owned_by_a_box && !in_group(other) && visited.insert(other) {
                            next.push(other);
                        }
                        continue;
                    };
                    if let Some(node) = fresh {
                        emitted_pins.extend(node.ports.iter().map(|p| p.id));
                        nodes.push(node);
                        if let Some(bx) = far_box {
                            emitted.insert(bx);
                        }
                    }
                    if followed {
                        kept += 1;
                    }
                    sig_pins.push(pin);
                    if edge_drives(e, near) {
                        drivers.push((pin, e.select.clone()));
                    } else {
                        loads.push((pin, e.select.clone()));
                    }
                    // A box seed exposes one pin per operand, so there is no single
                    // node the whole "signal" converges on and the drivers x loads
                    // join has nothing to cross. Wire this edge directly, seed pin
                    // to far pin, in the orientation the model gives.
                    if let Some(sc) = seed_scope {
                        let np = cone_pin_for(
                            design,
                            &mut scopes,
                            sc,
                            projection,
                            rep,
                            near,
                            e,
                            &mut pins,
                        );
                        if emitted_pins.contains(&np) {
                            let (dp, lp) = if edge_drives(e, near) {
                                (pin, np)
                            } else {
                                (np, pin)
                            };
                            join_signal(
                                design.node(rep),
                                &root,
                                &[(dp, e.select.clone())],
                                &[(lp, e.select.clone())],
                                &mut seen_pairs,
                                &mut next_edge,
                                &mut edges,
                            );
                        }
                    }
                    let Some(bx) = far_box.filter(|_| followed) else {
                        // A free-net anchor, or a connection the direction rejected:
                        // keep walking through the signal itself, the way the
                        // wall-crossing arm always did — but only when this hop was
                        // one the direction actually asked for.
                        if followed && !in_group(other) && visited.insert(other) {
                            next.push(other);
                        }
                        continue;
                    };
                    // A module boundary. An `Instance` node carries no edges of its
                    // own — connectivity attaches to its `Port` children — so the
                    // enqueue below finds nothing and the walk would stop at the
                    // wall forever, byte-identical at every further depth. Enqueue
                    // the port the edge actually landed on instead: its group picks
                    // up the backing net inside the module, which is where that
                    // module's own logic hangs (#285). One port, not all of them, so
                    // growth stays proportional to what was traced.
                    if other != bx
                        && matches!(
                            design.node(bx).map(|n| n.kind),
                            Some(NodeKind::Instance | NodeKind::Interface)
                        )
                    {
                        for m in group_of(&mut group_cache, design, other).iter().copied() {
                            if !in_group(m) && visited.insert(m) {
                                next.push(m);
                            }
                        }
                    }
                    // Next hop: this box's other signals — but never one the scope
                    // graph draws inline. `child_boxes` makes no box for those, so
                    // the next hop finds nothing on the far side and the one-sided
                    // anchor heuristic re-materializes them as phantom one-pin boxes
                    // duplicating the decoration already drawn (#268). A parameter
                    // tie is inline the same way, but only on a gate's own operand
                    // edge — elsewhere a `Param` is an ordinary signal.
                    for be in design.edges_of(bx) {
                        let bsig = if be.port == bx { be.endpoint } else { be.port };
                        let inline = is_drawn_inline(design, bsig, projection)
                            || (projection == Projection::GateLevel
                                && is_gate(design, be.port)
                                && is_kind(design, bsig, NodeKind::Param));
                        if !in_group(bsig) && !inline && visited.insert(bsig) {
                            next.push(bsig);
                        }
                    }
                }

                // A signal with reached boxes on only one side needs an anchor for
                // its wires — the seed net itself, most often. Added only when the
                // complementary side is empty, so it never duplicates a real
                // box-to-box wire.
                let mut stub_pin = None;
                let one_sided = drivers.is_empty() != loads.is_empty();
                // A signal that is itself a box (a `Memory` array is both) already
                // has real pins from its own builder; synthesizing a stub pin keyed
                // on its model id would name a pin that box never emitted. A node
                // the scope graph draws inline gets no stub either — the frontier
                // filter above keeps those out, and this covers a seed that is one.
                // Nor does a gate primitive: a stub stands in for a *signal* the
                // trace reaches, and at ProcessLevel a gate is interior to its block
                // — stubbing it drew a `Mux` the scope graph never shows (#268).
                //
                // Three reasons to want one, not one (#285). One-sided, as before.
                // A `crossing` — a port and its backing net — needs an anchor even
                // with both sides present, because it *is* the point where the two
                // sides meet; without it they are drawn as two disconnected islands
                // sharing a label. And a step's own seed always gets one, so a walk
                // that finds nothing still draws the signal it was asked about.
                if (one_sided || crossing || is_seed)
                    && !members.iter().any(|m| emitted.contains(m))
                    && !is_drawn_inline(design, rep, projection)
                    && !is_gate(design, rep)
                {
                    let stub_side = if drivers.is_empty() {
                        Side::East
                    } else {
                        Side::West
                    };
                    // Deliberately *not* charged against `limits.boxes`: a stub is a
                    // wire anchor, not a box, and starving it is what the `.max(1)`
                    // clamps above exist to prevent — with no anchor the one-sided
                    // join draws nothing, so a tight budget would return an empty
                    // graph instead of a small one. Stubs stay bounded anyway, since
                    // the signals reaching this point come from boxes the budget
                    // already capped.
                    if !stubs.contains(&rep) {
                        if let Some(node) = make_signal_stub(design, rep, stub_side) {
                            emitted_pins.extend(node.ports.iter().map(|p| p.id));
                            nodes.push(node);
                            stubs.insert(rep);
                        }
                    }
                    if stubs.contains(&rep) {
                        stub_pin = Some(rep);
                        // Only a one-sided signal hangs its anchor *in* a side; a
                        // crossing with both sides present is routed through it
                        // below instead, and a bare seed has no side to join.
                        if one_sided {
                            if drivers.is_empty() {
                                drivers.push((rep, None));
                            } else {
                                loads.push((rep, None));
                            }
                        }
                    }
                }

                if dropped > 0 {
                    truncated = true;
                    // Report the remainder where the user can see it: on the
                    // signal's own anchor when it has one, else on every pin the
                    // hop did emit for it.
                    match stub_pin {
                        Some(p) => *more_on.entry(p).or_default() += dropped,
                        None => {
                            for p in &sig_pins {
                                *more_on.entry(*p).or_default() += dropped;
                            }
                        }
                    }
                }
                debug_assert!(kept + dropped as usize <= total);

                // A box seed already wired every edge pin-to-pin above; crossing
                // its drivers with its loads here would wire its inputs straight
                // to its outputs, drawing a connection the model does not make.
                if seed_scope.is_some() {
                    continue;
                }
                match stub_pin.filter(|_| !one_sided && !drivers.is_empty() && !loads.is_empty()) {
                    // The port *is* the crossing point, so route through it rather
                    // than crossing drivers x loads: the logic inside the module and
                    // the logic outside it meet at one node instead of being drawn
                    // as two islands with the same label (#285). Also strictly fewer
                    // wires — |d| + |l| rather than |d| x |l|.
                    Some(anchor) => {
                        let mid = [(anchor, None)];
                        join_signal(
                            design.node(rep),
                            &root,
                            &drivers,
                            &mid,
                            &mut seen_pairs,
                            &mut next_edge,
                            &mut edges,
                        );
                        join_signal(
                            design.node(rep),
                            &root,
                            &mid,
                            &loads,
                            &mut seen_pairs,
                            &mut next_edge,
                            &mut edges,
                        );
                    }
                    None => join_signal(
                        design.node(rep),
                        &root,
                        &drivers,
                        &loads,
                        &mut seen_pairs,
                        &mut next_edge,
                        &mut edges,
                    ),
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
    }

    // Containment (#293): name the instance behind whose wall each box sits, and
    // stand up any container the walk did not otherwise emit.
    //
    // Before the collision reconciliation below, deliberately. A container is an
    // `Instance`, and `make_box` keys its pins on its own `Port` children — which
    // is exactly the model id a crossing anchor's stub is keyed on. Emitting the
    // container here lets that collision resolve the way it already does ("the
    // box wins"), which for a boundary is the *right* answer: the crossing point
    // degenerates into a pin on the container's wall, with the outside wired to
    // it from one side and the inside from the other. Emitting containers after
    // reconciliation would leave two nodes claiming one id, which no layout
    // engine can anchor.
    let mut containers: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    if !nodes.is_empty() {
        let mut want: Vec<NodeId> = Vec::new();
        for n in &nodes {
            let mut cur = containing_instance(design, n.id);
            while let Some(c) = cur {
                if !nodes.iter().any(|x| x.id == c) && !want.contains(&c) {
                    want.push(c);
                }
                cur = containing_instance(design, c);
            }
        }
        for c in want {
            let scope_path = design
                .node(c)
                .and_then(|n| n.parent)
                .and_then(|p| design.node(p))
                .map(|n| n.path.clone())
                .unwrap_or_else(|| root.clone());
            if let Some(node) = cone_box_node(design, c, &scope_path, &mut pins) {
                emitted_pins.extend(node.ports.iter().map(|p| p.id));
                nodes.push(node);
                emitted.insert(c);
                containers.insert(c);
            }
        }
        // Every container in every chain is on canvas now, so no `parent` can
        // name a node the renderer cannot find.
        for n in &mut nodes {
            n.parent = containing_instance(design, n.id);
        }
    }

    // A stub's pin id *is* its model id, so a signal some box also exposes as a
    // pin — an instance's own `Port` child, most often — ends up with that one id
    // on two nodes, which no layout engine can anchor. Reconciled here rather
    // than at stub time because neither order is knowable during the walk: the
    // box may be emitted before or after the stub, and refusing the stub up
    // front (on `emitted_pins`) also suppresses the bare-interface bundle anchor
    // #269 exists to keep.
    //
    // The box wins — it is the real design object, and every wire anchored on
    // that id stays valid precisely because it *is* the box's pin id, the same
    // number. Dropping the stub therefore loses no connectivity, only the
    // redundant second node claiming the same pin. On the committed golden this
    // is not hypothetical: six cone dumps carried the collision, and each loses
    // exactly one node here with its edge count unchanged.
    if !stubs.is_empty() {
        let box_pins: std::collections::HashSet<NodeId> = nodes
            .iter()
            .filter(|n| !stubs.contains(&n.id))
            .flat_map(|n| n.ports.iter().map(|p| p.id))
            .collect();
        nodes.retain(|n| {
            !stubs.contains(&n.id) || !n.ports.iter().any(|p| box_pins.contains(&p.id))
        });
    }

    if !more_on.is_empty() {
        for n in &mut nodes {
            for p in &mut n.ports {
                if let Some(&c) = more_on.get(&p.id) {
                    p.more = Some(c);
                }
            }
        }
    }

    // Drop anything left without a wire. A box is emitted as soon as one
    // connection anchors on it, but the signal-join only draws a wire where a
    // driver meets a load — so the last hop can leave a box whose every
    // connection was to a signal with nothing on the other side. Such a box
    // shows no connectivity at all, which is the whole content of this view;
    // an edge-less box on canvas reads as a claim about the design that the
    // model does not make (#269). A pin carrying a `more` count stays: that
    // count *is* the missing connectivity, reported rather than hidden.
    // A step's own seed is the exception (#285): it is not a claim about the
    // design, it is the thing the user pointed at, and drawing it with no wires
    // is the honest answer "nothing in this direction". Dropping it returned an
    // empty graph, which says "no such signal" instead.
    let wired: std::collections::HashSet<NodeId> =
        edges.iter().flat_map(|e| [e.source, e.target]).collect();
    nodes.retain(|n| {
        seed_anchors.contains(&n.id)
            || containers.contains(&n.id)
            || n.ports
                .iter()
                .any(|p| wired.contains(&p.id) || p.more.is_some())
    });
    // A container earns its place from what it holds, not from its own pins —
    // an instance the walk descended through is often wired entirely by the
    // boxes inside it. So it is exempt above and then dropped here if the pass
    // left it empty, which also unhooks anything that named it (#293). Repeated
    // until stable, because dropping an inner container can empty an outer one.
    loop {
        let holds: std::collections::HashSet<NodeId> =
            nodes.iter().filter_map(|n| n.parent).collect();
        let before = nodes.len();
        nodes.retain(|n| {
            !containers.contains(&n.id)
                || holds.contains(&n.id)
                || n.ports.iter().any(|p| wired.contains(&p.id))
        });
        // Anything whose container just went is no longer nested, or `parent`
        // would name a node the renderer cannot find.
        let live: std::collections::HashSet<NodeId> = nodes.iter().map(|n| n.id).collect();
        for n in &mut nodes {
            if n.parent.is_some_and(|p| !live.contains(&p)) {
                n.parent = None;
            }
        }
        if nodes.len() == before {
            break;
        }
    }

    // In a cone an unwired pin means "beyond the frontier", not "floating in the
    // design" — dimming it would state a falsehood the scope graph is entitled to
    // state and this view is not. `make_box` decides `dangling` from the *model*,
    // which is right for a scope and wrong here, and containment made it visible
    // by pulling in whole instances the walk never wired (#293).
    for n in &mut nodes {
        for p in &mut n.ports {
            p.dangling = false;
        }
    }

    SchematicGraph {
        root,
        nodes,
        edges,
        truncated,
    }
}

/// Fan-in/out cone of a node (typically a net or port): the boxes directly
/// connected to it, following edge direction up to `depth` hops.
///
/// Phase-3b extractor, retained deliberately: it is the `svxprobe graph --cone`
/// output contract and the `scale-bench` fan-out baseline that ADR 0003's
/// level-of-detail argument is measured against. New callers want [`cone_with`],
/// which fixes this one's direction filter and emits anchorable pin ids.
pub fn cone(design: &Design, start: NodeId, dir: Dir, depth: usize) -> SchematicGraph {
    let mut frontier = vec![start];
    let mut seen_boxes = std::collections::BTreeSet::new();
    let mut edges: Vec<SchEdge> = Vec::new();
    let mut seen_edge_ids = std::collections::BTreeSet::new();

    for _ in 0..depth.max(1) {
        let mut next = Vec::new();
        for &node in &frontier {
            for e in incident_edges(design, node) {
                // Direction filter: downstream follows 'out' from the node side.
                let keep = match dir {
                    Dir::Out => e.dir == Dir::Out,
                    Dir::In => e.dir == Dir::In,
                    Dir::Inout => true,
                };
                if !keep || !seen_edge_ids.insert(e.id) {
                    continue;
                }
                let endpoint = design.node(e.endpoint);
                let net =
                    endpoint.map(|n| with_select(last_segment(&n.path).to_string(), &e.select));
                let net_path = endpoint.map(|n| n.path.clone());
                edges.push(SchEdge {
                    id: e.id,
                    source: e.port,
                    target: e.endpoint,
                    net,
                    net_path,
                });
                let other = if e.port == node { e.endpoint } else { e.port };
                if let Some(parent_inst) = nearest_instance(design, other) {
                    if seen_boxes.insert(parent_inst) {
                        next.push(other);
                    }
                }
            }
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }

    let nodes = seen_boxes
        .into_iter()
        .filter_map(|b| make_box(design, b, ""))
        .collect();
    let root = design
        .node(start)
        .map(|n| n.path.clone())
        .unwrap_or_default();
    SchematicGraph {
        root,
        nodes,
        edges,
        truncated: false,
    }
}

fn incident_edges(design: &Design, node: NodeId) -> Vec<Edge> {
    design.edges_of(node).into_iter().cloned().collect()
}

/// The nearest enclosing Instance for a node (the box it belongs to).
fn nearest_instance(design: &Design, node: NodeId) -> Option<NodeId> {
    let mut cur = node;
    loop {
        let n = design.node(cur)?;
        if n.kind == NodeKind::Instance {
            return Some(cur);
        }
        cur = n.parent?;
    }
}
