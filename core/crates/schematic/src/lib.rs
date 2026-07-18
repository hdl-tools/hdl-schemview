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

use serde::Serialize;
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
}

/// Which projection [`scope_graph_with`]/[`expand_with`] render (#157, ADR 0005).
/// The default [`Projection::ProcessLevel`] is byte-identical to the bare
/// [`scope_graph`]/[`expand`] entry points — one box per `always`/`assign`.
/// [`Projection::GateLevel`] is the opt-in view: each combinational block dissolves
/// into its gate/mux primitive network. It never displaces the default — a UI
/// toggle selects it, and it only surfaces the primitives the harness's opt-in
/// `--gate-level` pass emits (a model without them renders identically either way).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
                .filter(|&c| is_gate(design, c))
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
    )
}

/// Whether `id` is a gate-level primitive node.
fn is_gate(design: &Design, id: NodeId) -> bool {
    design.node(id).is_some_and(|n| is_gate_kind(n.kind))
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
                id: pid,
                name: node.map(|n| n.name.clone()).unwrap_or_default(),
                side,
                path: node.map(|n| n.path.clone()).unwrap_or_default(),
                width: node.and_then(|n| pin_width(design, &n.type_)),
                role: None,
                bundle: false,
                dangling,
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
                id: vid,
                name,
                side,
                path: design.node(vid).map(|x| x.path.clone()).unwrap_or_default(),
                width: None,
                role: None,
                bundle: true,
                dangling: false,
            });
        }
        if let Some(side) = raw {
            ports.push(SchPort {
                id: RAW_PORT_BASE + bx,
                name: module_of(design, n).unwrap_or_else(|| n.name.clone()),
                side,
                path: n.path.clone(),
                width: None,
                role: None,
                bundle: true,
                dangling: false,
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
            id: c,
            name,
            side,
            path: cn.path.clone(),
            width: None,
            role: None,
            bundle: true,
            dangling: false,
        });
    }
    let label = relative_to(&n.path, scope);
    Some(SchNode {
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
        id,
        kind: NodeKind::Port,
        label: lit.to_string(),
        path: String::new(),
        expandable: false,
        // East-facing pin: drives rightward into the design.
        ports: vec![SchPort {
            id,
            name: lit.to_string(),
            side: Side::East,
            path: String::new(), // synthetic constant source — no model node
            width: None,
            role: None,
            bundle: false,
            dangling: false,
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
                id: pins.pin(ff, e.endpoint),
                name: sig.map(|s| s.name.clone()).unwrap_or_default(),
                side: side_of(e.dir),
                path: sig.map(|s| s.path.clone()).unwrap_or_default(),
                width: sig.and_then(|s| pin_width(design, &s.type_)),
                role,
                bundle: false,
                dangling: false,
            }
        })
        .collect();
    Some(SchNode {
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
                id: pins.pin(bx, e.endpoint),
                name: sig.map(|s| s.name.clone()).unwrap_or_default(),
                side: side_of(e.dir),
                path: sig.map(|s| s.path.clone()).unwrap_or_default(),
                width: sig.and_then(|s| pin_width(design, &s.type_)),
                role,
                bundle: false,
                dangling: false,
            }
        })
        .collect();
    let label = match n.kind {
        NodeKind::Assign => "assign",
        NodeKind::Latch => "latch",
        _ => "comb",
    };
    Some(SchNode {
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
                id: pins.pin(mem, e.endpoint),
                name: sig.map(|s| s.name.clone()).unwrap_or_default(),
                side: side_of(e.dir),
                path: sig.map(|s| s.path.clone()).unwrap_or_default(),
                width: sig.and_then(|s| pin_width(design, &s.type_)),
                role,
                bundle: false,
                dangling: false,
            }
        })
        .collect();
    Some(SchNode {
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
            let sig = design.node(e.endpoint);
            // The select input is the only role-tagged pin; the data branches
            // (and every non-mux operand) stay plain west pins.
            let role = matches!(e.mux_port, Some(MuxPort::Sel)).then_some(PinRole::Sel);
            SchPort {
                id: pins.pin(gate, e.endpoint),
                name: sig.map(|s| s.name.clone()).unwrap_or_default(),
                side: Side::West,
                path: sig.map(|s| s.path.clone()).unwrap_or_default(),
                width: sig.and_then(|s| pin_width(design, &s.type_)),
                role,
                bundle: false,
                dangling: false,
            }
        })
        .collect();
    // The single result pin, keyed on the gate itself so gate→gate and gate→signal
    // wires resolve to it in the signal-join pass. Carries the gate's own path so a
    // right-click cross-probes to its sub-expression `def_range`.
    ports.push(SchPort {
        id: pins.pin(gate, gate),
        name: String::new(),
        side: Side::East,
        path: n.path.clone(),
        width: None,
        role: None,
        bundle: false,
        dangling: false,
    });
    Some(SchNode {
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
        id: port,
        kind: NodeKind::Port,
        label: n.name.clone(),
        path: n.path.clone(),
        expandable: false,
        ports: vec![SchPort {
            id: port,
            name: n.name.clone(),
            side,
            path: n.path.clone(),
            width: pin_width(design, &n.type_),
            role: None,
            bundle: false,
            dangling: false,
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
                id: pin,
                name: m.name.clone(),
                side: side_of(m.dir),
                path: signode.map(|s| s.path.clone()).unwrap_or_default(),
                width: signode.and_then(|s| pin_width(design, &s.type_)),
                role: None,
                bundle: false,
                dangling: false,
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
            id: fid,
            kind: NodeKind::Port,
            label: mn.name.clone(),
            path: mn.path.clone(),
            expandable: false,
            ports: vec![SchPort {
                id: fid,
                name: mn.name.clone(),
                side,
                path: mn.path.clone(),
                width: None,
                role: None,
                bundle: true,
                dangling: false,
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
        root: scope_path.to_string(),
        nodes,
        edges,
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

    let boxes = child_boxes(design, scope, projection);
    let box_set: std::collections::HashSet<NodeId> = boxes.iter().copied().collect();
    // Synthesized pins (FF clk/data/Q) are handed out per (box, signal) so boxes
    // sharing a net keep distinct pins. The same allocator is reused by the FF
    // wiring branch below so wires resolve to the pins built here.
    let mut pins = PinAlloc::new();
    let mut nodes: Vec<SchNode> = Vec::new();
    for &b in &boxes {
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
    nodes.extend(
        own_ports
            .iter()
            .filter_map(|&p| make_boundary_pin(design, p)),
    );

    // Modport member pins: an in-scope modport-qualified interface port pins
    // its bundle members, and each pin's edge points at the underlying member —
    // a signal that lives *outside* this scope (in the interface instance), so
    // `box_of` can never anchor it. Map member -> [(bundle box, pin)] so wires
    // to bundle signals land on the pins instead of being dropped; several
    // bundles in one scope can view the same member, hence the Vec. BTreeMap
    // keeps the signal-join fold below deterministic.
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
    // retargeted onto the matching view's port in the edge fold below.
    // BTreeMap keeps the signal-join fold deterministic.
    let mut iface_owner: std::collections::BTreeMap<NodeId, NodeId> =
        std::collections::BTreeMap::new();
    let mut raw_port: std::collections::HashMap<NodeId, NodeId> = std::collections::HashMap::new();
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

    // Map an edge endpoint to (box-in-scope, endpoint-to-draw).
    let resolve = |node: NodeId| -> Option<(NodeId, NodeId)> {
        if let Some(&bp) = boundary_of.get(&node) {
            return Some((bp, bp));
        }
        // Anchor a bundle member on its pin only when one bundle views it —
        // with several, a structural edge has no single right pin (each pin
        // still wires by direction via the signal-join fold below).
        if let Some([(b, pin)]) = iface_pin.get(&node).map(Vec::as_slice) {
            return Some((*b, *pin));
        }
        let b = box_of(design, node, &box_set)?;
        // A modport-qualified interface port directly under the box — or one of
        // its member pins — anchors on the box's bundle pin (#106), collapsing
        // the port-level and member-level edges onto one anchor.
        let bundle_pin = |id: NodeId| {
            design.node(id).and_then(|n| {
                (n.kind == NodeKind::Interface && n.modport.is_some() && n.parent == Some(b))
                    .then_some(id)
            })
        };
        // Draw to the specific pin when the node is a Port directly under the
        // box; a bare bundle's interior anchors on its raw access port (#96,
        // falling back to the box when nothing taps members raw); otherwise
        // anchor to the box.
        let pin = if is_kind(design, node, NodeKind::Port)
            && design.node(node).and_then(|n| n.parent) == Some(b)
        {
            node
        } else if let Some(owner) = iface_owner.get(&node) {
            *raw_port.get(owner).unwrap_or(owner)
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
        };
        Some((b, pin))
    };

    // A modport-level connection anchors on the bundle's view port (#96): when
    // the far end is a modport-qualified bundle pin, the bare-interface end
    // moves from its raw/box anchor onto the port carried by the matching
    // `Modport` node — so `.bus(bus.mem)` wires bundle pin to `mem`, directly.
    let view_port = |b: NodeId, far_pin: NodeId| -> Option<NodeId> {
        if iface_owner.get(&b) != Some(&b) {
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
    };

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
        let mut seed: std::collections::HashSet<NodeId> = box_set.clone();
        for &b in &boxes {
            if let Some(bn) = design.node(b) {
                seed.extend(bn.children.iter().copied());
            }
        }
        seed.extend(boundary_of.keys().copied());
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
        if let (Some((sb, src)), Some((tb, tgt))) = (resolve(e.port), resolve(e.endpoint)) {
            let (src, tgt) = (
                view_port(sb, tgt).unwrap_or(src),
                view_port(tb, src).unwrap_or(tgt),
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
            let key = *boundary_of.get(&e.endpoint).unwrap_or(&e.endpoint);
            if is_logic_box(design, e.port) && box_set.contains(&e.port) {
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
            if is_gate(design, e.port) && box_set.contains(&e.port) {
                if e.dir == Dir::Out {
                    drivers
                        .entry(key)
                        .or_default()
                        .push((pins.pin(e.port, e.port), None));
                } else {
                    loads
                        .entry(key)
                        .or_default()
                        .push((pins.pin(e.port, e.endpoint), e.select.clone()));
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
            if !box_set.contains(&parent) || !is_kind(design, parent, NodeKind::Instance) {
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
        for &p in &own_ports {
            fold_anchor(p, p);
        }
        for (&sig, pins_of_sig) in &iface_pin {
            for &(_, pin) in pins_of_sig {
                fold_anchor(sig, pin);
            }
        }
        // Bundle members fold under their bundle's raw access port (#96): an
        // in-scope logic box reading or driving `bus.valid` wires to the
        // aggregate port. The synthetic pin has no model node, so it folds as
        // both a driver and a load — the aggregate carries traffic both ways.
        for (&member, &owner) in &iface_owner {
            if member == owner {
                continue;
            }
            if let Some(&rp) = raw_port.get(&owner) {
                fold_anchor(member, rp);
            }
        }
        // Every gate box offers its result under its own id key (#157): a non-root
        // gate feeding another gate has no `out` edge, so its output pin is offered
        // here, and the consumer's `in` edge (loaded under the producer's id above)
        // meets it. A root gate's self-key has no loads and is simply skipped.
        for &b in &boxes {
            if is_gate(design, b) {
                drivers.entry(b).or_default().push((pins.pin(b, b), None));
            }
        }
        // BTreeSet for a deterministic wire order across runs.
        let signals: std::collections::BTreeSet<NodeId> =
            drivers.keys().chain(loads.keys()).copied().collect();
        for sig in signals {
            let (Some(ds), Some(ls)) = (drivers.get(&sig), loads.get(&sig)) else {
                continue;
            };
            let label = design.node(sig).map(|n| relative_to(&n.path, scope_path));
            let net_path = design.node(sig).map(|n| n.path.clone());
            for &(dpin, ref dsel) in ds {
                for &(lpin, ref lsel) in ls {
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
                    edges.push(SchEdge {
                        id: next_edge,
                        source: dpin,
                        target: lpin,
                        net,
                        net_path: net_path.clone(),
                    });
                    next_edge += 1;
                }
            }
        }
    }

    // Constant tie-offs: a small source node outside each box, wired to every
    // input the model records as driven by a literal. ELK lays these out left of
    // the consumer and routes other wires around them.
    for &b in &boxes {
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

    // Mark dangling output pins on synthesized logic boxes (#118) — an FF/comb/
    // assign output that nothing in this scope reads stays visible (dimmed by
    // the frontend) instead of being pruned, so a write-only register still
    // shows its Q with the signal's name.
    let wired: std::collections::HashSet<NodeId> =
        edges.iter().flat_map(|e| [e.source, e.target]).collect();
    for node in &mut nodes {
        if is_logic_box(design, node.id) || is_gate(design, node.id) {
            for p in &mut node.ports {
                if p.side == Side::East && !wired.contains(&p.id) {
                    p.dangling = true;
                }
            }
        }
    }

    Some(SchematicGraph {
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

/// Fan-in/out cone of a node (typically a net or port): the boxes directly
/// connected to it, following edge direction up to `depth` hops.
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
    SchematicGraph { root, nodes, edges }
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
