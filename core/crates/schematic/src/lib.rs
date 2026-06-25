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

/// The boxes directly inside a scope: child `Instance`s and child `GenBlock`s
/// (a generate block/array is shown as an expandable group box).
fn child_boxes(design: &Design, scope: NodeId) -> Vec<NodeId> {
    design
        .node(scope)
        .map(|n| {
            n.children
                .iter()
                .copied()
                .filter(|&c| {
                    matches!(
                        design.node(c).map(|n| n.kind),
                        Some(NodeKind::Instance) | Some(NodeKind::GenBlock)
                    )
                })
                .collect()
        })
        .unwrap_or_default()
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

/// Build a box node with its ports; port sides come from incident edges.
fn make_box(design: &Design, bx: NodeId) -> Option<SchNode> {
    let n = design.node(bx)?;
    // Generate blocks have no ports; instances expose their Port children.
    let ports: Vec<SchPort> = n
        .children
        .iter()
        .copied()
        .filter(|&c| is_kind(design, c, NodeKind::Port))
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
    let label = if n.name.is_empty() {
        last_segment(&n.path).to_string()
    } else {
        n.name.clone()
    };
    Some(SchNode {
        id: bx,
        kind: n.kind,
        label,
        path: n.path.clone(),
        expandable: !child_boxes(design, bx).is_empty(),
        ports,
        module: module_of(design, n),
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
    let mut nodes: Vec<SchNode> = boxes.iter().filter_map(|&b| make_box(design, b)).collect();

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
        if let (Some((sb, src)), Some((tb, tgt))) = (resolve(e.port), resolve(e.endpoint)) {
            // Collapse parallel connections that land on the same two anchors
            // (e.g. both lanes' clk meeting one boundary pin).
            if sb != tb && seen.insert((src.min(tgt), src.max(tgt))) {
                let net = design
                    .node(e.endpoint)
                    .map(|n| relative_to(&n.path, scope_path));
                edges.push(SchEdge {
                    id: i as u32,
                    source: src,
                    target: tgt,
                    net,
                });
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
                    .map(|n| last_segment(&n.path).to_string());
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
        .filter_map(|b| make_box(design, b))
        .collect();
    let root = design
        .node(start)
        .map(|n| n.path.clone())
        .unwrap_or_default();
    SchematicGraph { root, nodes, edges }
}

fn incident_edges(design: &Design, node: NodeId) -> Vec<Edge> {
    design.edges_of(node).into_iter().copied().collect()
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
