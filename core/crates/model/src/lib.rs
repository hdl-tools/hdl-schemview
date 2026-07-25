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
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
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
    /// A SystemVerilog `interface` instance (a signal bundle) or a
    /// modport-specialized interface port on a consumer. Distinct from `Instance`
    /// so the schematic can draw a bundle shape rather than a module box.
    Interface,
    /// A named view (`modport`) of an interface bundle.
    Modport,
    /// A memory array (`logic [W-1:0] ram [0:N-1]`) — one modelled construct
    /// (the unpacked-dimension `Var`) rendered as a MEMORY glyph rather than a
    /// wire. Distinct from `Var` so the drilled logic view can draw an array
    /// with addr/din/dout/read/write pins. Process-granularity per ADR 0004 —
    /// the box maps to the array's `def_range`, so cross-probe stays a lookup.
    Memory,
    // --- Gate-level projection primitives (#157, ADR 0005) ------------------
    // Emitted only by the harness's opt-in `--gate-level` pass, which
    // decomposes process/assign RHS expressions into these primitives. Each is a
    // flat child of its process/assign node and carries a sub-expression
    // `def_range`, so cross-probe stays a lookup (the scoped ADR-0004
    // relaxation). Associative chains collapse to one N-input gate; `~` folds
    // onto the base gate (And→Nand, …); a `?:` becomes a `Mux`. The datapath
    // kinds (`Add`/`Sub`/`Mul`/`Cmp`/`Shift`) keep the exact operator on
    // `Node.op`, since the coarse kind can't tell `==` from `<`.
    /// N-input AND (also a reduction `&a`).
    And,
    /// N-input OR (also a reduction `|a`).
    Or,
    /// N-input XOR (also a reduction `^a`).
    Xor,
    /// N-input XNOR — an XOR with the output bubble folded on (`~^a`, `a ~^ b`).
    Xnor,
    /// N-input NAND — an AND with the output bubble folded on (`~(a & b)`, `~&a`).
    Nand,
    /// N-input NOR — an OR with the output bubble folded on (`~(a | b)`, `~|a`).
    Nor,
    /// Inverter — a standalone `~` over a bare signal (`~a`).
    Not,
    /// Buffer — an identity pass (`+a`), drawn as a bare triangle.
    Buf,
    /// Adder (`+`). Operator kept on `op`.
    Add,
    /// Subtractor (`-`). Operator kept on `op`.
    Sub,
    /// Multiplier / divide / mod / power. Exact operator kept on `op`.
    Mul,
    /// Comparator (`==`/`!=`/`<`/`>`/`<=`/`>=`). Exact operator kept on `op`.
    Cmp,
    /// Shifter (logical/arithmetic, left/right). Exact operator kept on `op`.
    Shift,
    /// Multiplexer — a `?:` conditional. Its select/data inputs are tagged on
    /// the edge via [`MuxPort`].
    Mux,
    /// A synthetic constant-operand source (#199): a hard-coded literal feeding a
    /// gate/mux/datapath input, carrying its value on [`Node::const_value`]. Emitted
    /// only by the `--gate-level` pass; the schematic renders it as a tie value on
    /// the input (like an instance-port constant tie), never as its own box.
    Const,
    /// A bit concatenation `{a, b, …}` (or replication `{n{a}}`) feeding a gate/mux
    /// input (#199): a primitive box gathering its element expressions, so a mux data
    /// branch that is a concat (`sel ? {x[31:2], 2'b00} : …`) renders with its input
    /// instead of vanishing. Drawn as a `{ }` box.
    Concat,
}

/// A point in a source file.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct Location {
    pub line: u32,
    pub col: u32,
    pub offset: u32,
}

/// A half-open source range, with `file` indexing [`Document::files`].
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct Range {
    pub file: u32,
    pub start: Location,
    pub end: Location,
}

/// One HLS provenance correspondence (#159): a span of generated RTL and the
/// high-level C/C++ span it came from. The bidirectional line-region link behind
/// C↔RTL source cross-probing — a lookup, never a heuristic. Populated by the
/// harness from the provenance comments HLS tools embed in generated RTL.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct SourceMapEntry {
    /// Span in a generated (SystemVerilog) file.
    pub generated: Range,
    /// Span in the high-level (C/C++) source file.
    pub source: Range,
}

/// What the elaboration resolved an identifier occurrence to (#225). Mirrors the
/// schema enum. Read off the symbol, never inferred from the token text — an
/// identifier the harness cannot classify emits no [`NameRef`] at all, so it stays
/// default-colored rather than guessed at.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum NameClass {
    Module,
    Instance,
    Port,
    Signal,
    Param,
    Type,
    EnumMember,
    Function,
    Interface,
    Modport,
    Genvar,
}

/// One identifier occurrence in a source file (#225): a declaration's own name token
/// or a resolved value reference.
///
/// The model otherwise carries spans for *declarations* only (`def_range` /
/// `inst_range`), which is why a click inside a process body could resolve no finer
/// than the enclosing block. Populated by the harness's opt-in `--name-refs` pass;
/// empty otherwise, so a model without it behaves exactly as before.
///
/// Deliberately carries **no [`NodeId`]**: one source span maps to N elaborated nodes
/// when a module is instantiated more than once. [`rel`](Self::rel) is the
/// instance-invariant half, and the enclosing instance comes from the click context at
/// resolve time.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct NameRef {
    pub file: u32,
    /// 1-based line. Line/col are line-ending independent, so the source pane renders
    /// against these.
    pub line: u32,
    /// 1-based byte column of the identifier's first character.
    pub col: u32,
    /// Byte offset, on the same basis as a node's `def_range` — the resolver's key.
    pub offset: u32,
    /// Byte length. A qualified reference spans the whole reference (`bus.valid`,
    /// `soc_pkg::XLEN`), not just its last segment.
    pub len: u32,
    pub class: NameClass,
    /// Symbol path relative to the enclosing elaborated scope (`clk`,
    /// `g_lane[0].bus.valid`), so one span serves every instantiation of its module.
    /// Empty means the token names the scope itself (a module definition's name). A
    /// symbol outside that scope (package parameter, cross-hierarchy reference) is
    /// stored absolute with a leading `/` — a character SV paths never contain.
    pub rel: String,
}

impl NameRef {
    /// Byte range `[start, end)` of the occurrence.
    pub fn span(&self) -> (usize, usize) {
        (self.offset as usize, (self.offset + self.len) as usize)
    }

    /// The absolute path this ref names, if it was stored absolute (`/soc_pkg::XLEN`).
    /// `None` for the ordinary scope-relative case, which needs a scope to resolve.
    pub fn absolute_path(&self) -> Option<&str> {
        self.rel.strip_prefix('/')
    }
}

/// One source file referenced by ranges.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct FileEntry {
    pub id: u32,
    pub path: String,
    /// Source language (#159): `"systemverilog"` for elaborated RTL, `"c"`/`"cpp"` for a
    /// high-level source referenced only by HLS provenance comments. `None` defaults to
    /// SystemVerilog. Lets the resolver route a cross-language probe and the frontend
    /// pick which pane renders the file.
    #[serde(default)]
    pub language: Option<String>,
}

/// A node in the elaborated hierarchy.
#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
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
    /// Literal or resolved parameter value tied to an input (e.g. `32'd0`): a `Port`
    /// input, a synthetic `Const` gate operand, or a `Param` referenced by a gate
    /// (#199). `None` if net-driven.
    #[serde(rename = "const", default)]
    pub const_value: Option<String>,
    /// View name on a modport-specialized interface port (e.g. `mem` for
    /// `mem_if.mem bus`); `None` otherwise.
    #[serde(default)]
    pub modport: Option<String>,
    /// Membership of a `Modport` node: each bundle member visible through the
    /// view, with its direction (slang `ModportPort.direction`). Descriptive
    /// metadata — the modport stays a view (no children/drivers/loads); the
    /// member's own node lives in the parent interface instance at
    /// `<parent path>.<name>` (see [`Design::modport_member_nodes`]).
    /// `None` on non-modport nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<ModportMember>>,
    /// Canonical path of an inferred FF's async-reset signal — a model fact
    /// from the harness (the timing-control edge whose signal the body reads),
    /// never a name guess (#59). `None` when the process has no async reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<String>,
    /// Canonical path of an inferred latch's enable signal (the top-level
    /// gating condition) — a model fact from the harness (#59).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable: Option<String>,
    /// Word count of a `Memory` array (the unpacked dimension size, e.g. `512`
    /// for `ram [0:511]`) — a structural fact from slang, used to label the
    /// MEMORY glyph. `None` on non-memory nodes (#112).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_depth: Option<u32>,
    /// `$readmemh`/`$readmemb` initializer argument for a `Memory` array (the
    /// source-file expression text, e.g. `INIT_FILE`) — presence drives the
    /// INIT marker on the glyph. A model fact from the harness's `initial`-block
    /// scan, not a logic node (ADR 0004 keeps `initial` non-logic). `None` when
    /// the memory has no `$readmem*` initializer (#112).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_source: Option<String>,
    /// Exact operator of a gate-level datapath primitive (#157, ADR 0005) — e.g.
    /// `"LessThan"` on a [`NodeKind::Cmp`] or `"LogicalShiftLeft"` on a
    /// [`NodeKind::Shift`]. The coarse `kind` groups these, so the precise
    /// operator rides here for the label. `None` on non-gate nodes (and on the
    /// bitwise/reduction gates, whose `kind` already names them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    #[serde(default)]
    pub drivers: Vec<NodeId>,
    #[serde(default)]
    pub loads: Vec<NodeId>,
}

/// One member of a modport view: the member signal's name and its direction
/// through that view.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub struct ModportMember {
    pub name: String,
    pub dir: Dir,
}

/// Provenance of a serialization.
#[derive(
    Debug, Clone, Default, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct Generator {
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub version: String,
}

/// Connection direction, from the module port's perspective.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    In,
    Out,
    Inout,
}

/// Role of a memory-access edge (#112): which port of a `Memory` glyph the edge
/// feeds. Set by the harness's bounded array-access classification (`ram[idx]`),
/// so the schematic can draw addr/din/dout pins wired to the real signals rather
/// than string-guessing. `None` on ordinary (non-memory-access) edges.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum MemPort {
    /// The array-index expression driving the memory address.
    Addr,
    /// The value written into the memory (`ram[idx] <= din`).
    Din,
    /// The value read out of the memory (`x <= ram[idx]`).
    Dout,
}

/// Which port of a gate-level [`NodeKind::Mux`] an edge feeds (#157, ADR 0005).
/// A `?:` decomposes to a mux whose three inputs are role-tagged so the
/// schematic can place the select on its own side and the data inputs in order.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum MuxPort {
    /// The condition expression selecting between the data inputs.
    Sel,
    /// The value chosen when the select is false (the `?:` else branch).
    D0,
    /// The value chosen when the select is true (the `?:` then branch).
    D1,
}

/// A port-connection edge: a module `port` wired to an external `endpoint`
/// (net / var / port / interface instance).
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
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
    /// Memory-access role of this edge (#112): set when the edge wires a signal
    /// to a `Memory` node's address/data port, so the schematic renders the
    /// pin's role (addr/din/dout). `None` for ordinary connectivity edges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_port: Option<MemPort>,
    /// Mux-port role of this edge (#157, ADR 0005): set when the edge feeds a
    /// gate-level [`NodeKind::Mux`]'s select/data input (sel/d0/d1), so the
    /// schematic places it correctly. `None` for ordinary connectivity edges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mux_port: Option<MuxPort>,
}

/// The deserialized Node-model document (matches `model.schema.json`).
#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct Document {
    pub schema_version: u32,
    pub design: String,
    #[serde(default)]
    pub generator: Generator,
    pub files: Vec<FileEntry>,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// Normalized enum table, keyed by canonical type string (matching a node's
    /// `type_`): value→name members for FSM/enum state display. Enums with
    /// non-literal members are omitted by the harness.
    #[serde(default)]
    pub enums: HashMap<String, EnumDef>,
    /// HLS C/C++ ↔ RTL provenance map (#159). Empty when the design carries no
    /// provenance comments (the default, so non-HLS output is unaffected).
    #[serde(default)]
    pub source_map: Vec<SourceMapEntry>,
    /// Identifier-occurrence spans (#225): every declaration name token and every
    /// resolved value reference, for semantic source coloring and usage → signal
    /// resolution. Empty unless the harness ran with `--name-refs`, so a model without
    /// it behaves exactly as before. Sorted by `(file, offset)`.
    #[serde(default)]
    pub name_refs: Vec<NameRef>,
}

/// An enum type's bit width and its value→name members.
#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct EnumDef {
    pub width: u32,
    pub members: Vec<EnumMember>,
}

/// One enum member: its declared name and encoded integer value.
#[derive(
    Debug, Clone, Serialize, Deserialize, rkyv::Archive, rkyv::Serialize, rkyv::Deserialize,
)]
pub struct EnumMember {
    pub name: String,
    pub value: u64,
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
    /// Rebuild the index from a flat `(node, signal)` list — the inverse of
    /// [`pairs`](Self::pairs). Lets the matcher's output be persisted as a plain
    /// `Vec` and restored without re-running the match (#153 wave cache).
    pub fn from_pairs(pairs: impl IntoIterator<Item = (NodeId, WaveSignalRef)>) -> Self {
        let mut idx = WaveIndex::default();
        for (node, sig) in pairs {
            idx.insert(node, sig);
        }
        idx
    }
    /// The `(node, signal)` bijection as a flat iterator — the serializable form
    /// of the index (both maps rebuild from it via [`from_pairs`](Self::from_pairs)).
    pub fn pairs(&self) -> impl Iterator<Item = (NodeId, WaveSignalRef)> + '_ {
        self.by_node.iter().map(|(&node, &sig)| (node, sig))
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
    /// Per-(generated RTL file) interval tree over `source_map[*].generated` offsets →
    /// `source_map` index. Answers "which C span does this RTL offset map to?" (#159).
    gen_map_index: HashMap<u32, Lapper<usize, usize>>,
    /// Per-(C/C++ file) interval tree over `source_map[*].source` offsets → `source_map`
    /// index. Answers "which RTL span does this C offset map to?" (#159).
    src_map_index: HashMap<u32, Lapper<usize, usize>>,
    /// Per-file interval tree over `name_refs` offsets → index into `doc.name_refs`
    /// (#225). Symmetric to `src_index`, but kept **separate** so it never changes
    /// `src_index`'s narrowest-covering-node outcome: a usage span is finer than the
    /// enclosing declaration and would silently alter every existing click-to-probe.
    name_ref_index: HashMap<u32, Lapper<usize, usize>>,
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

        // HLS provenance indices (#159): one interval tree per file over each side of
        // the source_map, mapping an offset → the source_map entry index. Symmetric to
        // src_index, so a C↔RTL probe is the same interval lookup.
        let mut gen_ivs: HashMap<u32, Vec<Interval<usize, usize>>> = HashMap::new();
        let mut src_ivs: HashMap<u32, Vec<Interval<usize, usize>>> = HashMap::new();
        for (i, m) in doc.source_map.iter().enumerate() {
            for (side, dest) in [(&m.generated, &mut gen_ivs), (&m.source, &mut src_ivs)] {
                let (lo, hi) = (side.start.offset as usize, side.end.offset as usize);
                let stop = if hi > lo { hi } else { lo + 1 };
                dest.entry(side.file).or_default().push(Interval {
                    start: lo,
                    stop,
                    val: i,
                });
            }
        }
        let gen_map_index = gen_ivs
            .into_iter()
            .map(|(f, ivs)| (f, Lapper::new(ivs)))
            .collect();
        let src_map_index = src_ivs
            .into_iter()
            .map(|(f, ivs)| (f, Lapper::new(ivs)))
            .collect();

        // Name-ref index (#225): one interval tree per file over the identifier spans.
        let mut nr_ivs: HashMap<u32, Vec<Interval<usize, usize>>> = HashMap::new();
        for (i, r) in doc.name_refs.iter().enumerate() {
            let (lo, hi) = r.span();
            let stop = if hi > lo { hi } else { lo + 1 };
            nr_ivs.entry(r.file).or_default().push(Interval {
                start: lo,
                stop,
                val: i,
            });
        }
        let name_ref_index = nr_ivs
            .into_iter()
            .map(|(f, ivs)| (f, Lapper::new(ivs)))
            .collect();

        Design {
            doc,
            path_index,
            src_index,
            conn_index,
            gen_map_index,
            src_map_index,
            name_ref_index,
            wave_index: WaveIndex::default(),
        }
    }

    pub fn nodes(&self) -> &[Node] {
        &self.doc.nodes
    }

    /// The enum definition for a node's `type_` string, if that type is an enum.
    pub fn enum_for_type(&self, type_: &str) -> Option<&EnumDef> {
        self.doc.enums.get(type_)
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.doc.nodes.get(id as usize)
    }

    /// Nodes at an exact canonical path.
    pub fn nodes_at_path(&self, path: &str) -> &[NodeId] {
        self.path_index.get(path).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Resolve a modport member (by name) to the underlying signal node(s).
    /// The member lives in the modport's parent interface instance, so its
    /// canonical path is `<parent path>.<name>` — a pure path lookup, no
    /// heuristics. Empty when `modport` has no parent or the path is unknown.
    pub fn modport_member_nodes(&self, modport: NodeId, member: &str) -> &[NodeId] {
        let parent = self
            .node(modport)
            .and_then(|n| n.parent)
            .and_then(|p| self.node(p));
        match parent {
            Some(p) => self.nodes_at_path(&format!("{}.{}", p.path, member)),
            None => &[],
        }
    }

    /// Nodes whose def/inst range covers `offset` in `file` (source → node).
    pub fn nodes_at_source(&self, file: u32, offset: usize) -> Vec<NodeId> {
        match self.src_index.get(&file) {
            Some(lap) => lap.find(offset, offset + 1).map(|iv| iv.val).collect(),
            None => Vec::new(),
        }
    }

    /// Nodes whose def/inst range overlaps `[lo, hi)` in `file`. Like
    /// [`nodes_at_source`](Self::nodes_at_source) but over a span — used to resolve a
    /// whole mapped line-region (#159) to the node(s) it contains, since an HLS
    /// provenance span covers a code line, not a single byte.
    pub fn nodes_in_source_range(&self, file: u32, lo: usize, hi: usize) -> Vec<NodeId> {
        match self.src_index.get(&file) {
            Some(lap) => lap.find(lo, hi.max(lo + 1)).map(|iv| iv.val).collect(),
            None => Vec::new(),
        }
    }

    /// The narrowest C/C++ span an RTL `offset` in a generated `file` maps to (#159),
    /// via the HLS provenance map. `None` when no provenance covers the offset.
    pub fn mapped_from_gen(&self, file: u32, offset: usize) -> Option<&Range> {
        self.narrowest_map(&self.gen_map_index, file, offset)
            .map(|m| &m.source)
    }

    /// The narrowest generated-RTL span a C/C++ `offset` in `file` maps to (#159), via
    /// the HLS provenance map. `None` when no provenance covers the offset. This is what
    /// turns a C-source click into an RTL node lookup.
    pub fn mapped_from_src(&self, file: u32, offset: usize) -> Option<&Range> {
        self.narrowest_map(&self.src_map_index, file, offset)
            .map(|m| &m.generated)
    }

    /// The `source_map` entry with the narrowest covering interval in `index` for
    /// `(file, offset)`. Narrowest wins so a fine-grained provenance line beats an
    /// enclosing coarse one, mirroring `from_source`'s innermost-node rule.
    fn narrowest_map(
        &self,
        index: &HashMap<u32, Lapper<usize, usize>>,
        file: u32,
        offset: usize,
    ) -> Option<&SourceMapEntry> {
        let lap = index.get(&file)?;
        lap.find(offset, offset + 1)
            .map(|iv| &self.doc.source_map[iv.val])
            .min_by_key(|m| {
                // width of the covering side that lives in `file`
                let r = if m.generated.file == file {
                    &m.generated
                } else {
                    &m.source
                };
                r.end.offset.saturating_sub(r.start.offset)
            })
    }

    /// The narrowest identifier occurrence covering `offset` in `file` (#225), or
    /// `None` if none does. Narrowest wins so a reference nested inside a wider one
    /// (`bus.valid` covers `valid`) resolves to the tightest match. The index is
    /// separate from `src_index`, so this never perturbs node source-resolution.
    pub fn name_ref_at(&self, file: u32, offset: usize) -> Option<&NameRef> {
        let lap = self.name_ref_index.get(&file)?;
        lap.find(offset, offset + 1)
            .map(|iv| &self.doc.name_refs[iv.val])
            .min_by_key(|r| r.len)
    }

    /// Every identifier occurrence in `file`, in `(file, offset)` order (#225). The
    /// bulk feed for the source pane's semantic coloring — one call per rendered file
    /// instead of a point probe per token.
    pub fn name_refs_in_file(&self, file: u32) -> Vec<&NameRef> {
        self.doc
            .name_refs
            .iter()
            .filter(|r| r.file == file)
            .collect()
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

    /// Positions into [`edges`](Self::edges) incident on `node` (as port or
    /// endpoint). These are the same `doc.edges` indices a full `enumerate()`
    /// scan would yield for `node`, so callers can gather a scope-local edge set
    /// without scanning every edge. Empty when `node` has no incident edges.
    pub fn edge_indices_of(&self, node: NodeId) -> &[u32] {
        self.conn_index.get(&node).map(Vec::as_slice).unwrap_or(&[])
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
            modport: None,
            members: None,
            reset: None,
            enable: None,
            mem_depth: None,
            init_source: None,
            op: None,
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
                language: None,
            }],
            nodes: vec![
                node(0, "t", NodeKind::Instance, Some(rng(0, 0, 10))),
                node(1, "t.a", NodeKind::Var, Some(rng(0, 4, 8))),
            ],
            edges: vec![],
            enums: HashMap::new(),
            source_map: Vec::new(),
            name_refs: Vec::new(),
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
    fn hls_source_map_lookup_both_directions() {
        // file 0 = generated RTL, file 1 = C source. One provenance entry links
        // RTL bytes 4..8 to C bytes 20..24.
        let doc = Document {
            schema_version: 1,
            design: "t".into(),
            generator: Generator::default(),
            files: vec![
                FileEntry {
                    id: 0,
                    path: "t.sv".into(),
                    language: Some("systemverilog".into()),
                },
                FileEntry {
                    id: 1,
                    path: "t.cpp".into(),
                    language: Some("cpp".into()),
                },
            ],
            nodes: vec![node(0, "t", NodeKind::Instance, Some(rng(0, 0, 10)))],
            edges: vec![],
            enums: HashMap::new(),
            source_map: vec![SourceMapEntry {
                generated: rng(0, 4, 8),
                source: rng(1, 20, 24),
            }],
            name_refs: Vec::new(),
        };
        let d = Design::from_document(doc);
        // RTL offset 5 → C span 20..24
        assert_eq!(d.mapped_from_gen(0, 5).map(|r| r.start.offset), Some(20));
        // C offset 22 → RTL span 4..8
        assert_eq!(d.mapped_from_src(1, 22).map(|r| r.start.offset), Some(4));
        // Offsets outside any provenance span map to nothing.
        assert!(d.mapped_from_gen(0, 9).is_none());
        assert!(d.mapped_from_src(1, 30).is_none());
        // Wrong-file lookups don't cross wires.
        assert!(d.mapped_from_gen(1, 22).is_none());
    }

    #[test]
    fn name_ref_lookup_is_narrowest_and_separate_from_src_index() {
        // Two refs on one file: a wide qualified reference `bus.valid` (4..13) and the
        // bare `valid` nested inside it (8..13). A declaration node covers 0..20.
        let doc = Document {
            schema_version: 1,
            design: "t".into(),
            generator: Generator::default(),
            files: vec![FileEntry {
                id: 0,
                path: "t.sv".into(),
                language: None,
            }],
            nodes: vec![node(0, "t", NodeKind::Instance, Some(rng(0, 0, 20)))],
            edges: vec![],
            enums: HashMap::new(),
            source_map: Vec::new(),
            name_refs: vec![
                NameRef {
                    file: 0,
                    line: 1,
                    col: 5,
                    offset: 4,
                    len: 9,
                    class: NameClass::Signal,
                    rel: "bus.valid".into(),
                },
                NameRef {
                    file: 0,
                    line: 1,
                    col: 9,
                    offset: 8,
                    len: 5,
                    class: NameClass::Signal,
                    rel: "bus.valid".into(),
                },
            ],
        };
        let d = Design::from_document(doc);
        // Offset 10 sits inside both spans → the narrower (len 5) wins.
        assert_eq!(d.name_ref_at(0, 10).map(|r| r.len), Some(5));
        // Offset 5 sits only inside the wide one.
        assert_eq!(d.name_ref_at(0, 5).map(|r| r.len), Some(9));
        // A name ref never leaks into node source-resolution: the only node here is `t`.
        assert_eq!(d.nodes_at_source(0, 10), vec![0]);
        // Bulk feed returns both refs for the file, none for an unknown file.
        assert_eq!(d.name_refs_in_file(0).len(), 2);
        assert!(d.name_refs_in_file(1).is_empty());
        // Absolute-path marker round-trips.
        assert_eq!(d.name_ref_at(0, 10).unwrap().absolute_path(), None);
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
                language: None,
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
                mem_port: None,
                mux_port: None,
            }],
            enums: HashMap::new(),
            source_map: Vec::new(),
            name_refs: Vec::new(),
        };
        let d = Design::from_document(doc);
        assert_eq!(d.edges().len(), 1);
        assert_eq!(d.edges_of(1).len(), 1);
        assert_eq!(d.edges_of(2)[0].dir, Dir::Out);
        assert!(d.edges_of(0).is_empty());
    }

    #[test]
    fn wave_index_round_trips_via_pairs() {
        // The matcher's durable output is a flat (node, signal) list; the two
        // lookup maps must be fully reconstructable from it (#153 wave cache).
        let mut wi = WaveIndex::default();
        wi.insert(3, WaveSignalRef(10));
        wi.insert(7, WaveSignalRef(20));

        let mut pairs: Vec<(NodeId, WaveSignalRef)> = wi.pairs().collect();
        pairs.sort_by_key(|(node, sig)| (*node, sig.0));
        assert_eq!(pairs, vec![(3, WaveSignalRef(10)), (7, WaveSignalRef(20))]);

        let rebuilt = WaveIndex::from_pairs(pairs);
        assert_eq!(rebuilt.len(), 2);
        assert_eq!(rebuilt.node_of(WaveSignalRef(10)), Some(3));
        assert_eq!(rebuilt.node_of(WaveSignalRef(20)), Some(7));
        assert_eq!(rebuilt.signal_of(3), Some(WaveSignalRef(10)));
        assert_eq!(rebuilt.signal_of(7), Some(WaveSignalRef(20)));
    }

    #[test]
    fn modport_member_nodes_resolve_via_parent_path() {
        // A Modport node carries membership metadata; each member's own node
        // lives in the parent interface instance at `<parent path>.<name>`.
        let mut bus = node(1, "t.bus", NodeKind::Interface, None);
        bus.parent = Some(0);
        bus.children = vec![2, 3];
        let mut valid = node(2, "t.bus.valid", NodeKind::Var, None);
        valid.parent = Some(1);
        let mut mp = node(3, "t.bus.mem", NodeKind::Modport, None);
        mp.parent = Some(1);
        mp.members = Some(vec![ModportMember {
            name: "valid".into(),
            dir: Dir::In,
        }]);
        let doc = Document {
            schema_version: 1,
            design: "t".into(),
            generator: Generator::default(),
            files: vec![],
            nodes: vec![node(0, "t", NodeKind::Instance, None), bus, valid, mp],
            edges: vec![],
            enums: HashMap::new(),
            source_map: Vec::new(),
            name_refs: Vec::new(),
        };
        let d = Design::from_document(doc);
        assert_eq!(d.modport_member_nodes(3, "valid"), &[2]);
        assert!(d.modport_member_nodes(3, "nope").is_empty());
        // The root has no parent: no members to resolve.
        assert!(d.modport_member_nodes(0, "valid").is_empty());
    }

    #[test]
    fn enum_for_type_looks_up_by_type_string() {
        let mut enums = HashMap::new();
        enums.insert(
            "p::e_t".to_string(),
            EnumDef {
                width: 2,
                members: vec![
                    EnumMember {
                        name: "A".into(),
                        value: 0,
                    },
                    EnumMember {
                        name: "B".into(),
                        value: 1,
                    },
                ],
            },
        );
        let doc = Document {
            schema_version: 1,
            design: "t".into(),
            generator: Generator::default(),
            files: vec![],
            nodes: vec![node(0, "t.s", NodeKind::Var, None)],
            edges: vec![],
            enums,
            source_map: Vec::new(),
            name_refs: Vec::new(),
        };
        let d = Design::from_document(doc);
        let e = d.enum_for_type("p::e_t").expect("enum present");
        assert_eq!(e.width, 2);
        assert_eq!(e.members[1].name, "B");
        assert_eq!(e.members[1].value, 1);
        assert!(d.enum_for_type("nope").is_none());
    }
}
