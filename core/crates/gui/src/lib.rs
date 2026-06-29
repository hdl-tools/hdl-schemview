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
use svxprobe_model::NodeId;
use svxprobe_schematic::{cone, expand, scope_graph, SchematicGraph};
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
            },
            WaveTarget::NotInTrace => WaveLink {
                in_trace: false,
                var_ref: 0,
                signal_ref: 0,
                full_name: String::new(),
            },
        }
    }
}
