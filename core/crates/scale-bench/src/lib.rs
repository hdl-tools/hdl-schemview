//! Synthetic elaborated-model generator for Phase-4 scalability benchmarks (#24).
//!
//! Emits a **valid** [`svxprobe_model::Document`] (one that passes
//! `svxprobe_ingest::from_slice`) at a target node count, with parameterized
//! hierarchy depth, per-net fan-out, and signal count. It is deterministic
//! (seeded SplitMix64, no entropy) so benchmark inputs are reproducible.
//!
//! The generator is measurement scaffolding, not shipping code: it lets the
//! criterion benches (and the `report` bin) exercise the eager load path,
//! scoped queries, `cone()` under a high-fan-out clock, and the matcher at
//! 665 / ~100K / ~1M nodes without needing a real 1M-node RTL design.
//!
//! **Hard invariant:** `Design::node(id)` is `doc.nodes[id as usize]`, so every
//! node's `id` equals its index in `doc.nodes`. The generator assigns ids from
//! the push position and never reorders (asserted in `tests/valid.rs`).

mod rng;

use rng::SplitMix64;
use svxprobe_model::{
    Design, Dir, Document, Edge, FileEntry, Generator, Location, Node, NodeId, NodeKind, Range,
};
use svxprobe_wave::WaveVar;

/// Knobs for a synthetic design. Deterministic for a given `seed`.
#[derive(Debug, Clone, Copy)]
pub struct SynthConfig {
    /// Approximate target total node count (geometry is derived to land near it).
    pub node_count: usize,
    /// Ordinary-net fan-out: edges emitted per leaf driver.
    pub fanout: usize,
    /// Instance-nesting depth below the root.
    pub depth: usize,
    /// Number of `WaveVar`s [`synth_signals`] produces for the matcher axis.
    pub signal_count: usize,
    /// PRNG seed — same seed ⇒ byte-identical output.
    pub seed: u64,
}

impl SynthConfig {
    /// ~665 nodes — mirrors the real `picorv32_soc` fixture baseline point.
    pub fn baseline() -> Self {
        Self {
            node_count: 665,
            fanout: 4,
            depth: 3,
            signal_count: 512,
            seed: 0xC0FF_EE00,
        }
    }

    /// ~100K nodes — the CI smoke ceiling.
    pub fn mid() -> Self {
        Self {
            node_count: 100_000,
            fanout: 8,
            depth: 4,
            signal_count: 20_000,
            seed: 0xC0FF_EE00,
        }
    }

    /// ~1M nodes — flagged (`SCALE_BENCH_FULL` / `--full`), not run by default.
    pub fn large() -> Self {
        Self {
            node_count: 1_000_000,
            fanout: 8,
            depth: 5,
            signal_count: 100_000,
            seed: 0xC0FF_EE00,
        }
    }
}

/// A generated design plus the handles the benches hit for representative work.
pub struct Synth {
    /// The valid document (passes `ingest::validate`).
    pub doc: Document,
    /// A global high-fan-out net — the `cone()` frontier-explosion case.
    pub clock_net: NodeId,
    /// Recorded degree of `clock_net` (for the report).
    pub clock_fanout: usize,
    /// A mid-depth instance path for `scope_graph`.
    pub mid_scope: String,
    /// Same instance, for `expand`.
    pub mid_instance: NodeId,
    /// An existing canonical Var path for `nodes_at_path`.
    pub probe_path: String,
    /// `(file, offset)` landing inside a node's range, for `nodes_at_source`.
    pub probe_source: (u32, usize),
}

struct Builder {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    off: u32,
    rng: SplitMix64,
}

impl Builder {
    fn new(seed: u64) -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            off: 0,
            rng: SplitMix64::new(seed),
        }
    }

    fn next_range(&mut self) -> Range {
        let lo = self.off;
        self.off += 10;
        let hi = lo + 8;
        Range {
            file: 0,
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

    fn push(
        &mut self,
        kind: NodeKind,
        name: &str,
        path: String,
        parent: Option<NodeId>,
        dir: Option<Dir>,
    ) -> NodeId {
        let id = self.nodes.len() as NodeId;
        let def_range = Some(self.next_range());
        self.nodes.push(Node {
            id,
            kind,
            name: name.to_string(),
            path: path.clone(),
            parent,
            children: Vec::new(),
            symbol_key: path,
            def_range,
            inst_range: None,
            type_: None,
            dir,
            const_value: None,
            modport: None,
            members: None,
            reset: None,
            enable: None,
            mem_depth: None,
            init_source: None,
            op: None,
            drivers: Vec::new(),
            loads: Vec::new(),
        });
        if let Some(p) = parent {
            self.nodes[p as usize].children.push(id);
        }
        id
    }

    fn edge(&mut self, port: NodeId, endpoint: NodeId, dir: Dir) {
        let id = self.edges.len() as u32;
        self.edges.push(Edge {
            id,
            port,
            endpoint,
            dir,
            select: None,
            mem_port: None,
            mux_port: None,
        });
    }
}

/// Approximate branching factor so a `depth`-level tree yields enough leaves to
/// hold the node budget. Always ≥ 2.
fn branching_for(node_count: usize, depth: usize) -> usize {
    let exp = 1.0 / ((depth + 1) as f64);
    ((node_count as f64).powf(exp) as usize).max(2)
}

/// Build a valid synthetic [`Document`] (+ handles) for `cfg`.
pub fn generate(cfg: &SynthConfig) -> Synth {
    let mut b = Builder::new(cfg.seed);
    let depth = cfg.depth.max(1);

    // Root instance "top" — id 0; also the matcher anchor root (Document.design).
    let root = b.push(NodeKind::Instance, "top", "top".to_string(), None, None);

    // Instance tree (BFS to `depth`), collecting the deepest instances as leaves.
    let branch = branching_for(cfg.node_count, depth);
    let mid_level = (depth / 2).max(1);
    let mut mid_instance = root;
    let mut mid_recorded = false;
    let mut leaves: Vec<NodeId> = Vec::new();
    let mut queue: std::collections::VecDeque<(NodeId, usize)> = std::collections::VecDeque::new();
    queue.push_back((root, 0));
    while let Some((pid, level)) = queue.pop_front() {
        if level >= depth {
            leaves.push(pid);
            continue;
        }
        for i in 0..branch {
            let name = format!("u{i}");
            let path = format!("{}.{}", b.nodes[pid as usize].path, name);
            let cid = b.push(NodeKind::Instance, &name, path, Some(pid), None);
            if !mid_recorded && level + 1 == mid_level {
                mid_instance = cid;
                mid_recorded = true;
            }
            queue.push_back((cid, level + 1));
        }
    }
    if leaves.is_empty() {
        leaves.push(root);
    }

    // Fill leaves round-robin with signal/logic nodes until near the budget.
    // Per-leaf order guarantees every leaf gets a Var (probe/signals) and a Ff
    // (clock fan-out) first; the din/dout Port+Net pairs exercise the
    // dual-node name-uniqueness whitelist.
    let mut per_leaf = vec![0usize; leaves.len()];
    let mut probe_path: Option<String> = None;
    let mut probe_source: Option<(u32, usize)> = None;
    let mut i = 0usize;
    while b.nodes.len() < cfg.node_count {
        let li = i % leaves.len();
        let leaf = leaves[li];
        let k = per_leaf[li];
        let base = b.nodes[leaf as usize].path.clone();
        match k {
            0 => {
                let path = format!("{base}.s0");
                let id = b.push(NodeKind::Var, "s0", path.clone(), Some(leaf), None);
                if probe_path.is_none() {
                    probe_path = Some(path);
                    let off = b.nodes[id as usize].def_range.unwrap().start.offset as usize;
                    probe_source = Some((0, off + 1));
                }
            }
            1 => {
                b.push(
                    NodeKind::Ff,
                    "q_reg",
                    format!("{base}.q_reg"),
                    Some(leaf),
                    None,
                );
            }
            2 => {
                b.push(
                    NodeKind::Port,
                    "din",
                    format!("{base}.din"),
                    Some(leaf),
                    Some(Dir::In),
                );
            }
            3 => {
                // Backing net for the `din` port — the dual-node pattern.
                b.push(
                    NodeKind::Net,
                    "din",
                    format!("{base}.din"),
                    Some(leaf),
                    None,
                );
            }
            4 => {
                b.push(
                    NodeKind::Port,
                    "dout",
                    format!("{base}.dout"),
                    Some(leaf),
                    Some(Dir::Out),
                );
            }
            5 => {
                b.push(
                    NodeKind::Net,
                    "dout",
                    format!("{base}.dout"),
                    Some(leaf),
                    None,
                );
            }
            _ => {
                let n = format!("s{}", k - 5);
                b.push(NodeKind::Var, &n, format!("{base}.{n}"), Some(leaf), None);
            }
        }
        per_leaf[li] += 1;
        i += 1;
    }

    // Ordinary edges: each leaf's first signal drives up to `fanout` of its
    // siblings — populates conn_index and gives scope_graph edges to scan.
    for &leaf in &leaves {
        let sigs: Vec<NodeId> = b.nodes[leaf as usize].children.clone();
        if let Some(&driver) = sigs.first() {
            for &load in sigs.iter().skip(1).take(cfg.fanout) {
                // Jitter the direction a little without breaking determinism.
                let dir = if b.rng.next_u64() & 1 == 0 {
                    Dir::Out
                } else {
                    Dir::In
                };
                b.edge(driver, load, dir);
            }
        }
    }

    // Global high-fan-out clock net under the root, wired to every leaf Ff with
    // an `Out` edge so `cone(clk, Dir::Out, _)` traverses the whole frontier.
    let clock_net = b.push(
        NodeKind::Net,
        "clk",
        "top.clk".to_string(),
        Some(root),
        None,
    );
    let mut clock_fanout = 0usize;
    let ff_ids: Vec<NodeId> = b
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Ff)
        .map(|n| n.id)
        .collect();
    for ff in ff_ids {
        b.edge(ff, clock_net, Dir::Out);
        clock_fanout += 1;
    }

    let mid_scope = b.nodes[mid_instance as usize].path.clone();
    let probe_path = probe_path.unwrap_or_else(|| b.nodes[root as usize].path.clone());
    let probe_source = probe_source.unwrap_or((0, 1));

    let doc = Document {
        schema_version: 1,
        design: "top".to_string(),
        generator: Generator {
            tool: "scale-bench".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        files: vec![FileEntry {
            id: 0,
            path: "synth.sv".to_string(),
            language: None,
        }],
        nodes: b.nodes,
        edges: b.edges,
        enums: std::collections::HashMap::new(),
        source_map: Vec::new(),
        name_refs: Vec::new(),
    };

    Synth {
        doc,
        clock_net,
        clock_fanout,
        mid_scope,
        mid_instance,
        probe_path,
        probe_source,
    }
}

/// Serialize a document to JSON bytes — the `from_slice` cold-load input.
pub fn to_json(doc: &Document) -> Vec<u8> {
    serde_json::to_vec(doc).expect("synthetic document serializes")
}

/// Generate and build a [`Design`] (skips `validate`; use for query/cone/matcher
/// setup where the input is already known-valid). Returns the `Synth` too, for
/// its handles.
pub fn build_design(cfg: &SynthConfig) -> (Design, Synth) {
    let synth = generate(cfg);
    let design = Design::from_document(synth.doc.clone());
    (design, synth)
}

/// `count` synthetic signals whose `full_name` is an existing Var path, so with
/// `MatchOptions.anchor = Some(vec!["top"])` they match ~100% deterministically.
/// `count` is independent of node count, letting the matcher axis sweep signal
/// count on its own.
pub fn synth_signals(synth: &Synth, _design: &Design, count: usize) -> Vec<WaveVar> {
    let var_paths: Vec<&str> = synth
        .doc
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Var)
        .map(|n| n.path.as_str())
        .collect();
    signals_from_paths(&var_paths, count)
}

/// Signals whose `full_name` is an existing `Var`/`Net`/`Port` path in a *loaded*
/// design (e.g. an elaborated real design from `SCALE_BENCH_MODEL`), so the
/// matcher axis works on a real basis too. Anchor with `design.doc.design`.
pub fn signals_from_design(design: &Design, count: usize) -> Vec<WaveVar> {
    let paths: Vec<&str> = design
        .nodes()
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Var | NodeKind::Net | NodeKind::Port))
        .map(|n| n.path.as_str())
        .collect();
    signals_from_paths(&paths, count)
}

fn signals_from_paths(paths: &[&str], count: usize) -> Vec<WaveVar> {
    if paths.is_empty() {
        return Vec::new();
    }
    (0..count)
        .map(|i| WaveVar {
            var_ref: i as u32,
            signal_ref: i as u32,
            full_name: paths[i % paths.len()].to_string(),
            is_parameter: false,
        })
        .collect()
}

/// Representative benchmark handles derived from an arbitrary loaded [`Design`]
/// (which carries no `Synth` metadata) — used for the real-design basis
/// (`claude_verilog_test` or any `SCALE_BENCH_MODEL`). Picks a mid-depth
/// instance with children for `scope_graph`, a signal path/source for the probe
/// axes, and the highest-degree net for the `cone()` frontier case.
#[derive(Debug, Clone)]
pub struct RealHandles {
    pub mid_scope: String,
    pub mid_instance: NodeId,
    pub probe_path: String,
    pub probe_source: (u32, usize),
    pub hot_net: NodeId,
    pub hot_fanout: usize,
}

/// Derive [`RealHandles`] from a loaded design.
pub fn derive_handles(design: &Design) -> RealHandles {
    let nodes = design.nodes();

    // Highest-degree node — the cone() frontier-explosion case.
    let mut degree: std::collections::HashMap<NodeId, usize> = std::collections::HashMap::new();
    for e in design.edges() {
        *degree.entry(e.port).or_default() += 1;
        if e.endpoint != e.port {
            *degree.entry(e.endpoint).or_default() += 1;
        }
    }
    let (hot_net, hot_fanout) = degree
        .iter()
        .max_by_key(|(_, d)| **d)
        .map(|(&n, &d)| (n, d))
        .unwrap_or((0, 0));

    // Mid-depth instance with children (so scope_graph draws boxes).
    let mut insts: Vec<&Node> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Instance && !n.children.is_empty())
        .collect();
    insts.sort_by_key(|n| n.path.matches('.').count());
    let mid = insts.get(insts.len() / 2).copied();
    let (mid_instance, mid_scope) = match mid {
        Some(n) => (n.id, n.path.clone()),
        None => (0, design.doc.design.clone()),
    };

    // A signal with a source range for the probe axes.
    let signal = nodes
        .iter()
        .find(|n| {
            matches!(n.kind, NodeKind::Var | NodeKind::Net | NodeKind::Port)
                && n.def_range.is_some()
        })
        .or_else(|| nodes.iter().find(|n| n.def_range.is_some()));
    let (probe_path, probe_source) = match signal {
        Some(n) => {
            let r = n.def_range.unwrap();
            (n.path.clone(), (r.file, r.start.offset as usize + 1))
        }
        None => (design.doc.design.clone(), (0, 0)),
    };

    RealHandles {
        mid_scope,
        mid_instance,
        probe_path,
        probe_source,
        hot_net,
        hot_fanout,
    }
}
