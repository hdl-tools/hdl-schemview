//! The elaborated Node model — the spine of hdl-schemview.
//!
//! Per the [roadmap](../../../docs/ROADMAP.md) §2, the elaborated hierarchy is
//! the single source of truth; source, schematic, and waveform are projections
//! of it. This crate defines the node types (deserialized from the pyslang
//! harness JSON) plus the three indices that make cross-probing a lookup:
//!
//! * `path_index`  — canonical path  → node(s)
//! * `src_index`   — source range    → node(s)   (one-to-many; interval tree)
//! * `wave_index`  — node ↔ waveform signal      (populated at trace load, Phase 1)
//!
//! Phase 0 builds `path_index` and `src_index`; `wave_index` is a skeleton.

use std::collections::HashMap;

use rust_lapper::{Interval, Lapper};
use serde::{Deserialize, Serialize};

/// Index of a node within [`Document::nodes`].
pub type NodeId = u32;

/// Kinds of spine node. Mirrors the schema enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    Instance,
    Net,
    Port,
    Var,
    Param,
    ModuleDef,
    GenBlock,
    /// Inferred sequential register (an edge-sensitive `always_ff` / clocked
    /// `always` block).
    #[serde(rename = "FF")]
    Ff,
    /// Combinational process (an `always_comb` / `always @*`).
    Comb,
    /// Level-sensitive latch (an `always_latch`). Distinct from `Comb` so the
    /// schematic can flag inferred latches (often unintended).
    Latch,
    /// Continuous `assign` — a combinational function driving one signal. Kept
    /// distinct from `Comb` so the schematic can render it as a function node.
    Assign,
}

/// A point in a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub line: u32,
    pub col: u32,
    pub offset: u32,
}

/// A half-open source range, with `file` indexing [`Document::files`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub file: u32,
    pub start: Location,
    pub end: Location,
}

/// One source file referenced by ranges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub id: u32,
    pub path: String,
}

/// A node in the elaborated hierarchy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub name: String,
    pub path: String,
    pub parent: Option<NodeId>,
    #[serde(default)]
    pub children: Vec<NodeId>,
    pub symbol_key: String,
    #[serde(default)]
    pub def_range: Option<Range>,
    #[serde(default)]
    pub inst_range: Option<Range>,
    #[serde(rename = "type", default)]
    pub type_: Option<String>,
    /// Declared direction for `Port` nodes (drives schematic pin side); `None`
    /// otherwise.
    #[serde(default)]
    pub dir: Option<Dir>,
    /// Literal value tied to a `Port` input (e.g. `32'd0`); `None` if net-driven.
    #[serde(rename = "const", default)]
    pub const_value: Option<String>,
    #[serde(default)]
    pub drivers: Vec<NodeId>,
    #[serde(default)]
    pub loads: Vec<NodeId>,
}

/// Provenance of a serialization.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Generator {
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub version: String,
}

/// Connection direction, from the module port's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    In,
    Out,
    Inout,
}

/// A port-connection edge: a module `port` wired to an external `endpoint`
/// (net / var / port / interface instance).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub id: u32,
    pub port: NodeId,
    pub endpoint: NodeId,
    pub dir: Dir,
    /// Resolved bit-select on the connected expression (e.g. `[0]` or `[7:4]`),
    /// used to label the wire with the bit it carries. `None` for a whole-signal
    /// connection. Emitted by the elaboration harness; never inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
}

/// The deserialized Node-model document (matches `model.schema.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub schema_version: u32,
    pub design: String,
    #[serde(default)]
    pub generator: Generator,
    pub files: Vec<FileEntry>,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

/// Opaque reference to a signal in a loaded waveform. Populated in Phase 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WaveSignalRef(pub u64);

/// Node ↔ waveform signal bijection. Skeleton in Phase 0.
#[derive(Debug, Default)]
pub struct WaveIndex {
    by_node: HashMap<NodeId, WaveSignalRef>,
    by_signal: HashMap<WaveSignalRef, NodeId>,
}

impl WaveIndex {
    pub fn insert(&mut self, node: NodeId, sig: WaveSignalRef) {
        self.by_node.insert(node, sig);
        self.by_signal.insert(sig, node);
    }
    pub fn node_of(&self, sig: WaveSignalRef) -> Option<NodeId> {
        self.by_signal.get(&sig).copied()
    }
    pub fn signal_of(&self, node: NodeId) -> Option<WaveSignalRef> {
        self.by_node.get(&node).copied()
    }
    pub fn len(&self) -> usize {
        self.by_node.len()
    }
    pub fn is_empty(&self) -> bool {
        self.by_node.is_empty()
    }
}

/// The elaborated design plus its lookup indices.
pub struct Design {
    pub doc: Document,
    /// Canonical path → node ids. One-to-many: a port and its backing variable
    /// can share a path (the reverse maps are explicitly many per the roadmap).
    path_index: HashMap<String, Vec<NodeId>>,
    /// Per-file interval tree over source offsets → node ids.
    src_index: HashMap<u32, Lapper<usize, NodeId>>,
    /// Node id → indices into `doc.edges` incident on that node (port or endpoint).
    conn_index: HashMap<NodeId, Vec<u32>>,
    pub wave_index: WaveIndex,
}

impl Design {
    /// Build a `Design` (and its indices) from a deserialized document.
    pub fn from_document(doc: Document) -> Self {
        let mut path_index: HashMap<String, Vec<NodeId>> = HashMap::new();
        let mut per_file: HashMap<u32, Vec<Interval<usize, NodeId>>> = HashMap::new();

        for node in &doc.nodes {
            path_index
                .entry(node.path.clone())
                .or_default()
                .push(node.id);
            for r in [node.def_range, node.inst_range].into_iter().flatten() {
                let (lo, hi) = (r.start.offset as usize, r.end.offset as usize);
                // Lapper needs stop > start; widen zero-length points by 1.
                let stop = if hi > lo { hi } else { lo + 1 };
                per_file.entry(r.file).or_default().push(Interval {
                    start: lo,
                    stop,
                    val: node.id,
                });
            }
        }

        let src_index = per_file
            .into_iter()
            .map(|(f, ivs)| (f, Lapper::new(ivs)))
            .collect();

        let mut conn_index: HashMap<NodeId, Vec<u32>> = HashMap::new();
        for (i, e) in doc.edges.iter().enumerate() {
            conn_index.entry(e.port).or_default().push(i as u32);
            if e.endpoint != e.port {
                conn_index.entry(e.endpoint).or_default().push(i as u32);
            }
        }

        Design {
            doc,
            path_index,
            src_index,
            conn_index,
            wave_index: WaveIndex::default(),
        }
    }

    pub fn nodes(&self) -> &[Node] {
        &self.doc.nodes
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.doc.nodes.get(id as usize)
    }

    /// Nodes at an exact canonical path.
    pub fn nodes_at_path(&self, path: &str) -> &[NodeId] {
        self.path_index.get(path).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Nodes whose def/inst range covers `offset` in `file` (source → node).
    pub fn nodes_at_source(&self, file: u32, offset: usize) -> Vec<NodeId> {
        match self.src_index.get(&file) {
            Some(lap) => lap.find(offset, offset + 1).map(|iv| iv.val).collect(),
            None => Vec::new(),
        }
    }

    pub fn path_count(&self) -> usize {
        self.path_index.len()
    }

    /// All connection edges (port ↔ endpoint) in the design.
    pub fn edges(&self) -> &[Edge] {
        &self.doc.edges
    }

    /// Edges incident on `node` (as either a port or an endpoint).
    pub fn edges_of(&self, node: NodeId) -> Vec<&Edge> {
        match self.conn_index.get(&node) {
            Some(ids) => ids.iter().map(|&i| &self.doc.edges[i as usize]).collect(),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: NodeId, path: &str, kind: NodeKind, range: Option<Range>) -> Node {
        Node {
            id,
            kind,
            name: path.rsplit('.').next().unwrap_or(path).to_string(),
            path: path.to_string(),
            parent: None,
            children: vec![],
            symbol_key: path.to_string(),
            def_range: range,
            inst_range: None,
            type_: None,
            dir: None,
            const_value: None,
            drivers: vec![],
            loads: vec![],
        }
    }

    fn rng(file: u32, lo: u32, hi: u32) -> Range {
        Range {
            file,
            start: Location {
                line: 1,
                col: 1,
                offset: lo,
            },
            end: Location {
                line: 1,
                col: 1,
                offset: hi,
            },
        }
    }

    #[test]
    fn path_and_source_lookup() {
        let doc = Document {
            schema_version: 1,
            design: "t".into(),
            generator: Generator::default(),
            files: vec![FileEntry {
                id: 0,
                path: "t.sv".into(),
            }],
            nodes: vec![
                node(0, "t", NodeKind::Instance, Some(rng(0, 0, 10))),
                node(1, "t.a", NodeKind::Var, Some(rng(0, 4, 8))),
            ],
            edges: vec![],
        };
        let d = Design::from_document(doc);
        assert_eq!(d.nodes_at_path("t.a"), &[1]);
        // offset 5 is inside both t (0..10) and t.a (4..8)
        let mut hit = d.nodes_at_source(0, 5);
        hit.sort();
        assert_eq!(hit, vec![0, 1]);
        // offset 9 is only inside t
        assert_eq!(d.nodes_at_source(0, 9), vec![0]);
    }

    #[test]
    fn conn_index_links_both_endpoints() {
        let doc = Document {
            schema_version: 1,
            design: "t".into(),
            generator: Generator::default(),
            files: vec![FileEntry {
                id: 0,
                path: "t.sv".into(),
            }],
            nodes: vec![
                node(0, "t", NodeKind::Instance, None),
                node(1, "t.p", NodeKind::Port, None),
                node(2, "t.n", NodeKind::Net, None),
            ],
            edges: vec![Edge {
                id: 0,
                port: 1,
                endpoint: 2,
                dir: Dir::Out,
                select: None,
            }],
        };
        let d = Design::from_document(doc);
        assert_eq!(d.edges().len(), 1);
        assert_eq!(d.edges_of(1).len(), 1);
        assert_eq!(d.edges_of(2)[0].dir, Dir::Out);
        assert!(d.edges_of(0).is_empty());
    }
}
