//! GUI session logic for hdl-schemview (roadmap Phase 3, the app's brain).
//!
//! Holds a loaded design + trace and answers the requests the three views make —
//! schematic graphs, cross-probe resolutions, source text, and signal values —
//! as serializable DTOs. UI-toolkit-free so it builds and tests in CI; the Tauri
//! shell in `app/src-tauri` is a thin wrapper that exposes these as commands.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use svxprobe_matcher::MatchOptions;
use svxprobe_model::{Design, EnumMember, NodeId, NodeKind};
use svxprobe_schematic::{cone, expand, module_of, scope_graph, SchematicGraph};
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
    pub wave: WaveLink,
    pub alternatives: Vec<NodeRef>,
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

/// A structural scope the hierarchy tree shows — the kinds `scope_graph`
/// accepts as roots, so clicking any tree node yields a schematic.
fn is_tree_scope(design: &Design, id: NodeId) -> bool {
    matches!(
        design.node(id).map(|n| n.kind),
        Some(NodeKind::Instance) | Some(NodeKind::GenBlock)
    )
}

/// A loaded design + trace: the state behind the GUI.
pub struct Session {
    cross: CrossProbe,
    wave: LoadedWave,
    src_root: PathBuf,
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
        let mut wave = LoadedWave::open(trace)?;
        let opts = MatchOptions {
            excluded_scopes: excluded,
            anchor: None,
        };
        let cross = CrossProbe::build(design, &wave, &opts);
        // Re-open: build() borrowed `wave`; we keep a fresh handle for lazy value
        // loading. (Opening twice is cheap — only the header is parsed.)
        wave = LoadedWave::open(trace)?;
        Ok(Self {
            cross,
            wave,
            src_root: src_root.as_ref().to_path_buf(),
        })
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

    fn tree_node(&self, id: NodeId, depth: usize) -> Option<TreeNode> {
        let design = self.cross.design();
        let n = design.node(id)?;
        let kids: Vec<NodeId> = n
            .children
            .iter()
            .copied()
            .filter(|&c| is_tree_scope(design, c))
            .collect();
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
        let full = self.src_root.join(&path);
        std::fs::read_to_string(&full).with_context(|| format!("reading {}", full.display()))
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
        ProbeResponse {
            anchor: self.node_ref(r.selection.anchor),
            source: self.source_loc(&r.selection),
            wave: self.wave_link(&r.selection),
            alternatives: r.alternatives.iter().map(|&id| self.node_ref(id)).collect(),
        }
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
