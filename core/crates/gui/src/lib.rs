//! GUI session logic for hdl-schemview (roadmap Phase 3, the app's brain).
//!
//! Holds a loaded design + trace and answers the requests the three views make —
//! schematic graphs, cross-probe resolutions, source text, and signal values —
//! as serializable DTOs. UI-toolkit-free so it builds and tests in CI; the Tauri
//! shell in `app/src-tauri` is a thin wrapper that exposes these as commands.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub mod startup;
use serde::Serialize;
pub use startup::{StartupArgs, StartupError};
use svxprobe_matcher::MatchOptions;
use svxprobe_model::{Design, EnumMember, NodeId, NodeKind};
use svxprobe_schematic::{
    cone, expand, expand_with, is_navigable_scope, module_of, pin_width, scope_graph,
    scope_graph_with,
};
// Re-exported so the shell and tests name the projection + graph DTOs through the
// session crate (the Tauri layer's single import surface), not the schematic crate.
pub use svxprobe_schematic::{Projection, SchematicGraph};
use svxprobe_wave::{LoadedWave, TraceTimescale, ValueChange};
use svxprobe_xprobe::{CrossProbe, Resolution, Selection, WaveTarget};

/// A reference to a model node, for the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct NodeRef {
    pub id: NodeId,
    pub path: String,
    pub kind: String,
}

/// Where the source view should scroll.
#[derive(Debug, Clone, Serialize)]
pub struct SourceLoc {
    pub file: u32,
    pub path: String,
    pub line: u32,
    /// Last line the construct spans (1-based, inclusive). The frontend highlights
    /// `line..=end_line` by line number — line numbers are line-ending-independent,
    /// so the highlight is correct whether the `def_range` byte offsets were computed
    /// against an LF or CRLF checkout (#203).
    pub end_line: u32,
    pub col: u32,
    pub offset: u32,
    pub end_offset: u32,
}

/// The waveform target for a selection.
#[derive(Debug, Clone, Serialize)]
pub struct WaveLink {
    pub in_trace: bool,
    pub var_ref: u32,
    pub signal_ref: u32,
    pub full_name: String,
    /// value→name members when the signal is enum-typed, for FSM state display (#81).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_map: Option<Vec<EnumMember>>,
}

/// A full cross-probe answer: the anchor, where each other view goes, and the
/// picker alternatives.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeResponse {
    pub anchor: NodeRef,
    pub source: Option<SourceLoc>,
    /// The cross-language counterpart location (#159): the C/C++ span that `source`'s
    /// generated-RTL span maps to via the HLS provenance map, when one exists. Lets the
    /// frontend highlight both the RTL and C panes from one probe. `None` when the design
    /// has no provenance map or the anchor's span isn't covered by it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapped_source: Option<SourceLoc>,
    pub wave: WaveLink,
    pub alternatives: Vec<NodeRef>,
}

/// One source file in the loaded design (#159), for the frontend to enumerate and open
/// — in particular the C/C++ files an HLS design references, so it can reveal a C
/// source pane. `language` mirrors `FileEntry::language` (`None` ⇒ SystemVerilog).
#[derive(Debug, Clone, Serialize)]
pub struct SourceFile {
    pub id: u32,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// One node of the lazy instance-hierarchy tree (#92): a structural scope with
/// its children populated down to the requested depth. `expandable` flags an
/// unexpanded node that has more levels below, so the frontend fetches them on
/// demand instead of loading the whole tree at startup.
#[derive(Debug, Clone, Serialize)]
pub struct TreeNode {
    /// Last path segment (e.g. `g_lane[0]`, `memory`).
    pub label: String,
    /// Canonical model path — feeds straight into `scope_graph`/`setScope`.
    pub path: String,
    /// Module/interface type sublabel (same recovery as schematic boxes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub expandable: bool,
    pub children: Vec<TreeNode>,
}

/// One signal declared directly inside a scope (#171) — a row of a waveform pane's
/// signal picker. The tree lists the design's scopes (`hierarchy_tree`); this lists
/// what is *inside* one, so a pane picks its own lanes without borrowing another
/// window's tree.
///
/// Every field is the cross-probe's own answer for `path` rather than a
/// re-derivation, so a row and the `probe_node` that follows it cannot disagree.
/// Note there is deliberately no `dir`: a modport member's direction is relative to
/// the view reading it, so one signal's equivalence set carries contradictory
/// directions (`out` of a producer's view, `in` to a consumer's). No single model
/// answer ⇒ no field.
#[derive(Debug, Clone, Serialize)]
pub struct SignalEntry {
    /// Canonical model path — the picker's key, and what a click probes.
    pub path: String,
    /// Last path segment, as declared.
    pub name: String,
    /// The *cross-probe's* representative kind for this path. `best_kind` ranks the
    /// backing `Var` above its `Port`, so a module port reports `Var` — that names
    /// the node a click actually selects, which is the point.
    pub kind: NodeKind,
    /// Declared bit-range (`[31:0]`), enum width fallback included; `None` for a
    /// scalar. Annotated by the schematic's own `pin_width`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<String>,
    /// Whether this session's trace carries the signal. False → the frontend dims the
    /// row rather than pruning it: the model has the signal, the trace doesn't.
    pub in_trace: bool,
}

/// The kinds the picker lists: real signal declarations a trace can carry.
///
/// `Param` is excluded — an elaboration-time constant is never in a trace, so it
/// could only ever be a permanently-dimmed row. `Instance`/`GenBlock` and a bare
/// `Interface` are the *tree's* scopes, `Modport` is a view of a bundle, and
/// `Ff`/`Comb`/`Latch`/`Assign` are synthesized boxes with no declaration of their
/// own. Excluding `Interface` also keeps the picker from recursing into a
/// modport-qualified port, whose children's paths live in a *different* scope.
fn is_signal_kind(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::Port | NodeKind::Net | NodeKind::Var | NodeKind::Memory
    )
}

/// A structural scope the hierarchy tree shows — exactly the roots `scope_graph`
/// accepts, so clicking any tree node yields a schematic. This is the schematic
/// crate's `is_navigable_scope` verbatim (Instances, *navigable* generate blocks, and
/// drillable bare interface bundles), reused rather than re-derived so the tree and
/// the schematic can never disagree on what is navigable. In particular a generate
/// block that holds only logic is not a scope (#184): its contents render dissolved
/// into the enclosing module, so a row for it would be a redundant, dead click.
fn is_tree_scope(design: &Design, id: NodeId) -> bool {
    is_navigable_scope(design, id)
}

/// A generate-for's array container: a named `GenBlock` (`g_lane`) whose
/// children are the unnamed per-iteration `GenBlock`s (`g_lane[0]`, …). The
/// tree hoists these one level (#107) — iterations show, the container
/// doesn't — mirroring the schematic's GenBlock dissolve but keeping the
/// iterations as navigable scopes. Named if/case generate blocks have no
/// unnamed GenBlock children and stay visible.
fn is_gen_array_container(design: &Design, id: NodeId) -> bool {
    design.node(id).is_some_and(|n| {
        n.kind == NodeKind::GenBlock
            && !n.name.is_empty()
            && n.children.iter().any(|&c| {
                design
                    .node(c)
                    .is_some_and(|k| k.kind == NodeKind::GenBlock && k.name.is_empty())
            })
    })
}

/// The tree children of a scope: its structural-scope children, with each
/// generate-array container replaced by its iteration scopes.
fn tree_children(design: &Design, children: &[NodeId]) -> Vec<NodeId> {
    let mut out = Vec::new();
    for &c in children {
        if !is_tree_scope(design, c) {
            continue;
        }
        if is_gen_array_container(design, c) {
            if let Some(cn) = design.node(c) {
                out.extend(
                    cn.children
                        .iter()
                        .copied()
                        .filter(|&g| is_tree_scope(design, g)),
                );
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Choose the on-disk path for a model source file. An absolute path is used as-is;
/// otherwise `src_root/path` is preferred, but when that is missing and the stored
/// `path` exists (already relative to the process CWD — e.g. a designlist whose model
/// records `../../fixtures/...`), the stored path wins. This stops a mismatched src
/// root from double-applying `../..` into a non-existent path. When neither exists the
/// joined path is kept, so the "source unavailable" error names the configured
/// location. The existence check is injected so the decision is unit-testable.
fn pick_source_path(src_root: &Path, path: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let joined = src_root.join(path);
    if exists(&joined) || !exists(path) {
        joined
    } else {
        path.to_path_buf()
    }
}

/// A loaded design + trace: the state behind the GUI.
pub struct Session {
    cross: CrossProbe,
    wave: LoadedWave,
    src_root: PathBuf,
    /// The model file the design was ingested from — `None` when it was elaborated
    /// in-memory (no on-disk path, so no wave_index cache key). Kept, with `opts`,
    /// so [`load_trace`](Self::load_trace) can re-match a new trace against this
    /// same design (#176).
    model: Option<PathBuf>,
    opts: MatchOptions,
}

impl Session {
    /// Load a design (Node-model JSON), a trace, and the excluded scopes; `src_root`
    /// is the base dir for resolving the model's source file paths.
    pub fn load(
        model: &str,
        trace: &str,
        excluded: Vec<String>,
        src_root: impl AsRef<Path>,
    ) -> Result<Self> {
        let design = svxprobe_ingest::from_path(model)?;
        Self::from_design(design, Some(model), trace, excluded, src_root)
    }

    /// Elaborate a designlist with the external pyslang harness, then load the
    /// resulting model (#93). Runs `svxprobe-elaborate` (which must be on
    /// `PATH`; bundling is future packaging work) as a subprocess — the
    /// roadmap's out-of-process boundary — captures the model JSON from its
    /// stdout, and feeds it to the same ingest path as [`Session::load`]. No
    /// persistent cache: every call re-elaborates (caching is #21/#22).
    pub fn elaborate_and_load(
        filelist: &str,
        top: &str,
        incdirs: &[String],
        trace: &str,
        excluded: Vec<String>,
        src_root: impl AsRef<Path>,
        hls_src: &[String],
    ) -> Result<Self> {
        // Without --top the harness would silently auto-select one; require an
        // explicit name so the guard below compares against the user's intent.
        anyhow::ensure!(
            !top.trim().is_empty(),
            "a top module name is required to elaborate a designlist"
        );
        let mut cmd = std::process::Command::new("svxprobe-elaborate");
        cmd.arg("--top").arg(top).arg("-f").arg(filelist);
        for dir in incdirs {
            cmd.arg("-I").arg(dir);
        }
        // Always emit the gate-level projection (#157), like the committed golden. It is
        // additive — process-level rendering ignores the primitives — but the frontend's
        // gate-level toggle switches the view at scope_graph time without re-elaborating,
        // so the primitives must already be in the model. Without this, a designlist-
        // loaded design shows combinational logic as opaque Comb/Assign blocks even with
        // the toggle on, since there are no gates to dissolve.
        cmd.arg("--gate-level");
        // Declared C/C++ sources (#222) drive the HLS provenance pass. `--hls-map` is
        // passed *only* when sources are declared: it regex-scans every line of every
        // RTL file, which a pure-RTL design gains nothing from, and declaring C sources
        // is itself the opt-in. (Before #222 the flag was never passed at all, so a
        // designlist-loaded HLS design produced no source_map and could not cross-probe
        // to its C sources.)
        if !hls_src.is_empty() {
            cmd.arg("--hls-map");
            for path in hls_src {
                cmd.arg("--hls-src").arg(path);
            }
        }
        cmd.arg("-o").arg("-"); // model JSON on stdout; progress goes to stderr
        let out = cmd.output().context(
            "running svxprobe-elaborate (is the elaboration harness installed and on PATH?)",
        )?;
        if !out.status.success() {
            anyhow::bail!(
                "svxprobe-elaborate failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let design = svxprobe_ingest::from_slice(&out.stdout)?;
        // A top that doesn't exist elaborates to an empty design with exit 0;
        // catch that here instead of loading a blank session, and surface
        // whatever the harness said on stderr.
        anyhow::ensure!(
            design.doc.design == top,
            "elaboration produced no design for top '{top}' — check the top module name \
             and the filelist. Harness output: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        // No model file on disk (elaborated in-memory), so no wave_index cache key.
        Self::from_design(design, None, trace, excluded, src_root)
    }

    fn from_design(
        design: Design,
        model: Option<&str>,
        trace: &str,
        excluded: Vec<String>,
        src_root: impl AsRef<Path>,
    ) -> Result<Self> {
        let mut wave = LoadedWave::open(trace)?;
        let opts = MatchOptions {
            excluded_scopes: excluded,
            anchor: None,
        };
        // With a model file on disk, reuse the persisted wave_index (skips the
        // matcher on repeat launches, #153); otherwise match fresh.
        let cross = match model {
            Some(m) => {
                CrossProbe::build_cached(design, &wave, &opts, Path::new(m), Path::new(trace))
            }
            None => CrossProbe::build(design, &wave, &opts),
        };
        // Re-open: build() borrowed `wave`; we keep a fresh handle for lazy value
        // loading. (Opening twice is cheap — only the header is parsed.)
        wave = LoadedWave::open(trace)?;
        Ok(Self {
            cross,
            wave,
            src_root: src_root.as_ref().to_path_buf(),
            model: model.map(PathBuf::from),
            opts,
        })
    }

    /// Swap this session's trace, reusing the already-ingested design (#176).
    ///
    /// The design is unchanged, so only the trace and its matching are rebuilt: a
    /// waveform pane's "Load trace…" (#170) no longer re-ingests a model — or, worse,
    /// re-runs the elaboration harness — for a design that did not change. Cross-probe
    /// resolution stays a model lookup; the new trace is re-matched through the same
    /// Phase-1 matcher.
    ///
    /// The trace is opened *before* any state changes, so a bad path leaves the
    /// session intact and still serving its current trace.
    pub fn load_trace(&mut self, trace: &str) -> Result<()> {
        let probe = LoadedWave::open(trace)?;
        // With the model on disk, the new trace's index is cached like a fresh load's
        // (#153); an in-memory (elaborated) design has no cache key, so it matches.
        match &self.model {
            Some(m) => self
                .cross
                .rematch_cached(&probe, &self.opts, m, Path::new(trace)),
            None => self.cross.rematch(&probe, &self.opts),
        }
        // Re-open: rematch borrowed `probe`; keep a fresh handle for lazy value
        // loading, mirroring `from_design`.
        self.wave = LoadedWave::open(trace)?;
        Ok(())
    }

    pub fn design_top(&self) -> String {
        self.cross.design().doc.design.clone()
    }

    /// Top-level scopes to seed the schematic (the design top).
    pub fn list_scopes(&self) -> Vec<NodeRef> {
        let top = &self.cross.design().doc.design;
        self.cross
            .design()
            .nodes_at_path(top)
            .iter()
            .map(|&id| self.node_ref(id))
            .collect()
    }

    pub fn scope_graph(&self, scope: &str) -> Option<SchematicGraph> {
        scope_graph(self.cross.design(), scope)
    }

    /// [`scope_graph`] at an explicit [`Projection`] (#157 PR4). The bare method is
    /// exactly this at [`Projection::ProcessLevel`]; the shell forwards the frontend's
    /// gate-level toggle here.
    pub fn scope_graph_with(&self, scope: &str, projection: Projection) -> Option<SchematicGraph> {
        scope_graph_with(self.cross.design(), scope, projection)
    }

    /// The instance-hierarchy tree under `scope`, `depth` levels deep (#92).
    /// Tree nodes are the structural scopes (`Instance` / `GenBlock` — the same
    /// kinds `scope_graph` accepts as roots, so every node is navigable); the
    /// frontend expands lazily by re-calling with a child's path. `None` when
    /// `scope` names no structural node.
    pub fn hierarchy_tree(&self, scope: &str, depth: usize) -> Option<TreeNode> {
        let design = self.cross.design();
        let root = design
            .nodes_at_path(scope)
            .iter()
            .copied()
            .find(|&id| is_tree_scope(design, id))?;
        self.tree_node(root, depth)
    }

    /// The signals declared directly inside `scope` (#171) — the rows of a waveform
    /// pane's signal picker, listed in declaration order (matching the source; a sort
    /// would be one more place to diverge). `None` when `scope` names no structural
    /// scope, so the shell 404s it exactly as `hierarchy_tree` does; `Some(vec![])`
    /// for a real scope that declares nothing.
    ///
    /// Roots are **every** structural node at the path, not the first: a path can
    /// carry several (the fixture's `core.genblk1` is three `GenBlock`s, only one of
    /// which holds children), and a tree row carries only a path — so the union is
    /// the only answer consistent with whichever identical row the user clicked.
    /// This deliberately diverges from `hierarchy_tree`'s `find`.
    ///
    /// Children are never recursed. Beyond scope-vs-signal, an `Interface` port's
    /// children carry the *underlying members'* paths, which live in another scope
    /// entirely — recursing would list a neighbour's signals here.
    pub fn scope_signals(&self, scope: &str) -> Option<Vec<SignalEntry>> {
        let design = self.cross.design();
        let roots: Vec<NodeId> = design
            .nodes_at_path(scope)
            .iter()
            .copied()
            .filter(|&id| is_tree_scope(design, id))
            .collect();
        if roots.is_empty() {
            return None;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut out = Vec::new();
        for root in roots {
            let Some(rn) = design.node(root) else {
                continue;
            };
            for &c in &rn.children {
                let Some(n) = design.node(c) else { continue };
                if !is_signal_kind(n.kind) {
                    continue;
                }
                // Every port is two nodes at one path — the `Port` and its backing
                // `Var` (the dual-node pattern `ingest` whitelists). One signal, one
                // row: dedupe on the canonical path, which is what identifies it.
                if !seen.insert(n.path.as_str()) {
                    continue;
                }
                if let Some(e) = self.signal_entry(&n.path) {
                    out.push(e);
                }
            }
        }
        Some(out)
    }

    /// One picker row, answered by the cross-probe itself rather than read off the
    /// child node: `from_node_path` picks the equivalence set's representative
    /// (`best_kind` — the backing `Var` over its `Port`) and `to_wave` answers the
    /// trace question across the *whole* set. Asking the child directly would report
    /// `in_trace: false` for a dual-node `Port` whose `Var` is the matched node — a
    /// row that contradicts its own click.
    fn signal_entry(&self, path: &str) -> Option<SignalEntry> {
        let r = self.cross.from_node_path(path)?;
        let design = self.cross.design();
        let n = design.node(r.selection.anchor)?;
        Some(SignalEntry {
            path: n.path.clone(),
            name: n.name.clone(),
            kind: n.kind,
            width: pin_width(design, &n.type_),
            in_trace: matches!(self.cross.to_wave(&r.selection), WaveTarget::Linked { .. }),
        })
    }

    fn tree_node(&self, id: NodeId, depth: usize) -> Option<TreeNode> {
        let design = self.cross.design();
        let n = design.node(id)?;
        let kids = tree_children(design, &n.children);
        let children = if depth > 0 {
            kids.iter()
                .filter_map(|&c| self.tree_node(c, depth - 1))
                .collect()
        } else {
            Vec::new()
        };
        Some(TreeNode {
            label: n.path.rsplit('.').next().unwrap_or(&n.path).to_string(),
            path: n.path.clone(),
            module: module_of(design, n),
            expandable: !kids.is_empty(),
            children,
        })
    }

    pub fn expand(&self, node: NodeId) -> Option<SchematicGraph> {
        expand(self.cross.design(), node)
    }

    /// [`expand`] at an explicit [`Projection`] (#157 PR4) — the drill-down twin of
    /// [`scope_graph_with`]. The bare method is this at [`Projection::ProcessLevel`].
    pub fn expand_with(&self, node: NodeId, projection: Projection) -> Option<SchematicGraph> {
        expand_with(self.cross.design(), node, projection)
    }

    pub fn cone(&self, net: NodeId, dir: &str, depth: usize) -> SchematicGraph {
        use svxprobe_model::Dir;
        let d = match dir {
            "in" => Dir::In,
            "out" => Dir::Out,
            _ => Dir::Inout,
        };
        cone(self.cross.design(), net, d, depth)
    }

    pub fn source_text(&self, file: u32) -> Result<String> {
        let path = self
            .cross
            .design()
            .doc
            .files
            .iter()
            .find(|f| f.id == file)
            .map(|f| f.path.clone())
            .with_context(|| format!("unknown file id {file}"))?;
        let full = self.resolve_source(&path);
        std::fs::read_to_string(&full).with_context(|| format!("reading {}", full.display()))
    }

    /// Every source file in the loaded design, with its language (#159). The frontend
    /// uses this to reveal a C/C++ source pane when the design references non-SV files
    /// (an HLS flow) and to populate that pane's file picker.
    pub fn source_files(&self) -> Vec<SourceFile> {
        self.cross
            .design()
            .doc
            .files
            .iter()
            .map(|f| SourceFile {
                id: f.id,
                path: f.path.clone(),
                language: f.language.clone(),
            })
            .collect()
    }

    /// Resolve a model file path against the source root, robustly. An absolute path is
    /// used as-is; otherwise `src_root/path` is preferred, but when that does not exist
    /// and the path resolves as stored (already relative to the process CWD — e.g. a
    /// designlist whose model records `../../fixtures/...` — the stored path is used.
    /// This stops a mismatched src root from double-applying `../..` into a path that
    /// points nowhere (the `src_root/../../fixtures/...` "source unavailable" error).
    fn resolve_source(&self, path: &str) -> PathBuf {
        pick_source_path(&self.src_root, Path::new(path), |p| p.exists())
    }

    pub fn signal_values(&mut self, signal_ref: u32) -> Vec<ValueChange> {
        self.wave.signal_values(signal_ref)
    }

    /// The trace's timescale (tick → physical time), for time-axis labelling.
    pub fn trace_timescale(&self) -> Option<TraceTimescale> {
        self.wave.timescale()
    }

    // -- cross-probe --------------------------------------------------------

    pub fn probe_signal(
        &mut self,
        full_name: &str,
        context: Option<&str>,
    ) -> Option<ProbeResponse> {
        self.set_context(context);
        let r = self.cross.from_signal(full_name)?;
        Some(self.response(r))
    }

    pub fn probe_node(&mut self, path: &str, context: Option<&str>) -> Option<ProbeResponse> {
        self.set_context(context);
        let r = self.cross.from_node_path(path)?;
        Some(self.response(r))
    }

    pub fn probe_source(
        &mut self,
        file: u32,
        offset: usize,
        context: Option<&str>,
    ) -> Option<ProbeResponse> {
        self.set_context(context);
        let r = self.cross.from_source(file, offset)?;
        Some(self.response(r))
    }

    // -- helpers ------------------------------------------------------------

    fn set_context(&mut self, context: Option<&str>) {
        match context.and_then(|c| self.cross.design().nodes_at_path(c).first().copied()) {
            Some(id) => self.cross.set_context(id),
            None => self.cross.clear_context(),
        }
    }

    fn node_ref(&self, id: NodeId) -> NodeRef {
        let n = self.cross.design().node(id);
        NodeRef {
            id,
            path: n.map(|n| n.path.clone()).unwrap_or_default(),
            kind: n.map(|n| format!("{:?}", n.kind)).unwrap_or_default(),
        }
    }

    fn response(&self, r: Resolution) -> ProbeResponse {
        let source = self.source_loc(&r.selection);
        // The C/C++ counterpart of the (RTL) anchor's span, via the provenance map (#159).
        let mapped_source = source.as_ref().and_then(|s| self.mapped_source_loc(s));
        ProbeResponse {
            anchor: self.node_ref(r.selection.anchor),
            source,
            mapped_source,
            wave: self.wave_link(&r.selection),
            alternatives: r.alternatives.iter().map(|&id| self.node_ref(id)).collect(),
        }
    }

    /// The C/C++ `SourceLoc` a generated-RTL `SourceLoc` maps to, via the HLS provenance
    /// map (#159). A pure lookup off the model spine — `None` when the design carries no
    /// provenance or the RTL span isn't covered.
    fn mapped_source_loc(&self, rtl: &SourceLoc) -> Option<SourceLoc> {
        let r = *self
            .cross
            .design()
            .mapped_from_gen(rtl.file, rtl.offset as usize)?;
        let path = self
            .cross
            .design()
            .doc
            .files
            .iter()
            .find(|f| f.id == r.file)
            .map(|f| f.path.clone())
            .unwrap_or_default();
        Some(SourceLoc {
            file: r.file,
            path,
            line: r.start.line,
            end_line: r.end.line,
            col: r.start.col,
            offset: r.start.offset,
            end_offset: r.end.offset,
        })
    }

    fn source_loc(&self, sel: &Selection) -> Option<SourceLoc> {
        let st = self.cross.to_source(sel);
        let r = st.def.or(st.inst)?;
        let path = self
            .cross
            .design()
            .doc
            .files
            .iter()
            .find(|f| f.id == r.file)
            .map(|f| f.path.clone())
            .unwrap_or_default();
        Some(SourceLoc {
            file: r.file,
            path,
            line: r.start.line,
            end_line: r.end.line,
            col: r.start.col,
            offset: r.start.offset,
            end_offset: r.end.offset,
        })
    }

    fn wave_link(&self, sel: &Selection) -> WaveLink {
        match self.cross.to_wave(sel) {
            WaveTarget::Linked { var_ref, full_name } => WaveLink {
                in_trace: true,
                var_ref,
                signal_ref: self
                    .cross
                    .wave_var(var_ref)
                    .map(|v| v.signal_ref)
                    .unwrap_or(0),
                full_name,
                enum_map: self.enum_members(sel.anchor),
            },
            WaveTarget::NotInTrace => WaveLink {
                in_trace: false,
                var_ref: 0,
                signal_ref: 0,
                full_name: String::new(),
                enum_map: None,
            },
        }
    }

    /// The enum members for a node's declared type, when that type is an enum.
    fn enum_members(&self, node: NodeId) -> Option<Vec<EnumMember>> {
        let design = self.cross.design();
        let type_ = design.node(node)?.type_.as_deref()?;
        design.enum_for_type(type_).map(|e| e.members.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::pick_source_path;
    use std::path::{Path, PathBuf};

    // The bug (#204/#205 follow-up): a designlist model records `../../fixtures/x.sv`,
    // already resolvable from the CWD, but a stale `src_root` double-applies `../..`.
    // The stored path must win when the joined path misses.
    #[test]
    fn falls_back_to_stored_path_when_joined_is_missing() {
        let src = Path::new("../..");
        let p = Path::new("../../fixtures/rtl/x.sv");
        // Only the stored path exists on disk.
        let picked = pick_source_path(src, p, |c| c == p);
        assert_eq!(picked, PathBuf::from("../../fixtures/rtl/x.sv"));
    }

    #[test]
    fn prefers_joined_path_when_it_exists() {
        let src = Path::new("/root");
        let p = Path::new("fixtures/rtl/x.sv");
        let joined = PathBuf::from("/root/fixtures/rtl/x.sv");
        let picked = pick_source_path(src, p, |c| c == joined);
        assert_eq!(picked, joined);
    }

    #[test]
    fn keeps_joined_path_for_the_error_when_neither_exists() {
        let src = Path::new("/root");
        let p = Path::new("fixtures/rtl/x.sv");
        let picked = pick_source_path(src, p, |_| false);
        assert_eq!(picked, PathBuf::from("/root/fixtures/rtl/x.sv"));
    }

    #[test]
    fn absolute_path_is_used_as_is() {
        let abs = Path::new("/abs/rtl/x.sv");
        let picked = pick_source_path(Path::new("/root"), abs, |_| false);
        assert_eq!(picked, PathBuf::from("/abs/rtl/x.sv"));
    }
}
