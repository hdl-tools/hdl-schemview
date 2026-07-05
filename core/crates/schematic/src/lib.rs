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
use svxprobe_model::{Design, Dir, Edge, NodeId, NodeKind};

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
fn child_boxes(design: &Design, scope: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    if let Some(n) = design.node(scope) {
        for &c in &n.children {
            match design.node(c).map(|x| x.kind) {
                Some(NodeKind::Instance)
                | Some(NodeKind::Interface)
                | Some(NodeKind::Ff)
                | Some(NodeKind::Comb)
                | Some(NodeKind::Latch)
                | Some(NodeKind::Assign) => out.push(c),
                Some(NodeKind::GenBlock) => out.extend(child_boxes(design, c)),
                _ => {}
            }
        }
    }
    out
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
/// continuous assign) — the boxes the signal-join pass wires through shared nets.
fn is_logic_box(design: &Design, id: NodeId) -> bool {
    matches!(
        design.node(id).map(|n| n.kind),
        Some(NodeKind::Ff) | Some(NodeKind::Comb) | Some(NodeKind::Latch) | Some(NodeKind::Assign)
    )
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
fn pin_width(design: &Design, type_: &Option<String>) -> Option<String> {
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
    for e in design.edges() {
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
        // I/O frame). Other box kinds are drillable only if they contain boxes.
        expandable: n.kind == NodeKind::Instance || !child_boxes(design, bx).is_empty(),
        ports,
        module: module_of(design, n),
        constant: None,
        modport: n.modport.clone(),
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
    })
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
    })
}

/// The schematic of one scope: child-instance boxes wired by the connections
/// whose two ends both live inside the scope. Connections that leave the scope
/// (e.g. to a top-level clock) are omitted here; use [`cone`] to trace those.
pub fn scope_graph(design: &Design, scope_path: &str) -> Option<SchematicGraph> {
    let scope = *design.nodes_at_path(scope_path).iter().find(|&&id| {
        matches!(
            design.node(id).map(|n| n.kind),
            Some(NodeKind::Instance) | Some(NodeKind::GenBlock)
        )
    })?;

    let boxes = child_boxes(design, scope);
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

    let mut edges = Vec::new();
    let mut seen: std::collections::HashSet<(NodeId, NodeId)> = std::collections::HashSet::new();
    for (i, e) in design.edges().iter().enumerate() {
        // Logic boxes wire through the scope-level signals they read and assign;
        // that is done by the signal-join pass below. Here we draw only the
        // structural (instance) connections, both ends resolving to in-scope boxes.
        if is_logic_box(design, e.port) {
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
                    id: i as u32,
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
        for e in design.edges() {
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
        if is_logic_box(design, node.id) {
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

/// Expand an instance: the scope graph one level down inside it.
pub fn expand(design: &Design, instance: NodeId) -> Option<SchematicGraph> {
    let path = design.node(instance)?.path.clone();
    scope_graph(design, &path)
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
