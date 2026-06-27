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

/// A pin on a box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchPort {
    pub id: NodeId,
    pub name: String,
    pub side: Side,
    /// Bit-range of the pin (`[31:0]`) parsed from its declared type, or `None`
    /// for a scalar. Shown next to the pin label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<String>,
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
}

/// Synthetic id base for constant-source nodes (keyed by the driven port id), so
/// they never collide with real model node/port ids.
const CONST_ID_BASE: NodeId = 1 << 28;

/// Synthetic id base for synthesized logic-box pins (FFs today, combinational
/// boxes next). Above `CONST_ID_BASE` and any real model NodeId; pins are handed
/// out per `(box, signal)` by [`PinAlloc`] so boxes sharing a net keep distinct
/// pins.
const LOGIC_PIN_BASE: NodeId = 1 << 30;

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

/// The boxes shown inside a scope: child `Instance`s, with `GenBlock`s dissolved
/// — their leaf instances are pulled up so e.g. both `g_lane[*].core` sit
/// together at the top instead of behind a generate-array group box.
fn child_boxes(design: &Design, scope: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    if let Some(n) = design.node(scope) {
        for &c in &n.children {
            match design.node(c).map(|x| x.kind) {
                Some(NodeKind::Instance) | Some(NodeKind::Ff) => out.push(c),
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
fn module_of(design: &Design, node: &svxprobe_model::Node) -> Option<String> {
    if node.kind != NodeKind::Instance {
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

/// Build a box node with its ports; port sides come from incident edges. The
/// label is scope-relative (like wire labels) so flattened generate-block
/// iterations stay distinct — e.g. `g_lane[0].core` vs `g_lane[1].core`.
fn make_box(design: &Design, bx: NodeId, scope: &str) -> Option<SchNode> {
    let n = design.node(bx)?;
    // Generate blocks have no ports; instances expose their Port children.
    let ports: Vec<SchPort> = n
        .children
        .iter()
        .copied()
        .filter(|&c| is_kind(design, c, NodeKind::Port))
        // Show wired pins and constant-tied inputs; dangling ports (unconnected
        // outputs like mem_la_*, pcpi_*) are hidden to keep blocks compact.
        .filter(|&pid| {
            design.edges_of(pid).iter().any(|e| e.port == pid)
                || design.node(pid).is_some_and(|n| n.const_value.is_some())
        })
        .map(|pid| {
            let node = design.node(pid);
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
                .map(side_of)
                .unwrap_or(Side::West);
            SchPort {
                id: pid,
                name: node.map(|n| n.name.clone()).unwrap_or_default(),
                side,
                width: node.and_then(|n| width_of(&n.type_)),
            }
        })
        .collect();
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
            width: None,
        }],
        module: None,
        constant: Some(lit.to_string()),
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
            SchPort {
                id: pins.pin(ff, e.endpoint),
                name: sig.map(|s| s.name.clone()).unwrap_or_default(),
                side: side_of(e.dir),
                width: sig.and_then(|s| width_of(&s.type_)),
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
    })
}

/// A boundary I/O pin for one of the scope's own ports. Rendered as a frame pin
/// (no box). Its single pin faces the design — inputs enter from the west frame
/// (pin on the east), outputs leave at the east frame (pin on the west) — so the
/// layered layout places inputs on the left and outputs on the right.
fn make_boundary_pin(design: &Design, port: NodeId) -> Option<SchNode> {
    let n = design.node(port)?;
    let side = match n.dir {
        Some(Dir::Out) => Side::West,
        _ => Side::East,
    };
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
            width: width_of(&n.type_),
        }],
        module: None,
        constant: None,
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
        let node = if is_kind(design, b, NodeKind::Ff) {
            make_ff_box(design, b, scope_path, &mut pins)
        } else {
            make_box(design, b, scope_path)
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

    // Map an edge endpoint to (box-in-scope, endpoint-to-draw).
    let resolve = |node: NodeId| -> Option<(NodeId, NodeId)> {
        if let Some(&bp) = boundary_of.get(&node) {
            return Some((bp, bp));
        }
        let b = box_of(design, node, &box_set)?;
        // Draw to the specific port if the node is a Port directly under the box;
        // otherwise anchor to the box.
        let pin = if is_kind(design, node, NodeKind::Port)
            && design.node(node).and_then(|n| n.parent) == Some(b)
        {
            node
        } else {
            b
        };
        Some((b, pin))
    };

    let mut edges = Vec::new();
    let mut seen: std::collections::HashSet<(NodeId, NodeId)> = std::collections::HashSet::new();
    for (i, e) in design.edges().iter().enumerate() {
        // An FF wires through its synthesized pin (allocated per (ff, signal) in
        // make_ff_box above); other edges resolve both ends to in-scope boxes/pins.
        let src_res = if is_kind(design, e.port, NodeKind::Ff) {
            box_set
                .contains(&e.port)
                .then(|| (e.port, pins.pin(e.port, e.endpoint)))
        } else {
            resolve(e.port)
        };
        if let (Some((sb, src)), Some((tb, tgt))) = (src_res, resolve(e.endpoint)) {
            // Collapse parallel connections that land on the same two anchors
            // (e.g. both lanes' clk meeting one boundary pin).
            if sb != tb && seen.insert((src.min(tgt), src.max(tgt))) {
                let net = design
                    .node(e.endpoint)
                    .map(|n| with_select(relative_to(&n.path, scope_path), &e.select));
                edges.push(SchEdge {
                    id: i as u32,
                    source: src,
                    target: tgt,
                    net,
                });
            }
        }
    }

    // Constant tie-offs: a small source node outside each box, wired to every
    // input the model records as driven by a literal. ELK lays these out left of
    // the consumer and routes other wires around them.
    let mut next_edge = design.edges().len() as u32;
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
                });
                next_edge += 1;
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
                let net = design
                    .node(e.endpoint)
                    .map(|n| with_select(last_segment(&n.path).to_string(), &e.select));
                edges.push(SchEdge {
                    id: e.id,
                    source: e.port,
                    target: e.endpoint,
                    net,
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
