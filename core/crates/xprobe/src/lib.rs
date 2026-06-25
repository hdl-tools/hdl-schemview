//! The cross-probe engine (roadmap Phase 2 — source ↔ waveform).
//!
//! One selection channel links two projections of the elaborated model:
//! source position ⇄ waveform signal. A source position is inherently one-to-many
//! (generate-unrolled instances share a source range), so a source resolution
//! carries an `anchor` plus `alternatives` (the picker), disambiguated against the
//! active hierarchy `context`. A waveform signal resolves to a unique node.
//!
//! The engine is headless and in-process: views call these methods and render the
//! results. It reuses the Phase 1 matcher to populate the node ↔ signal index.

use std::collections::{BTreeMap, HashMap};

use svxprobe_matcher::{run_match, MatchOptions, MatchReport};
use svxprobe_model::{Design, NodeId, NodeKind, Range, WaveSignalRef};
use svxprobe_wave::{LoadedWave, WaveVar};

/// An equivalence set of nodes for one logical object, with the primary `anchor`.
/// (A path can carry a Port + its backing Var + a Net — the same signal.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub nodes: Vec<NodeId>,
    pub anchor: NodeId,
}

/// A resolved selection plus the picker `alternatives` (other distinct-path
/// candidates for an ambiguous source position).
#[derive(Debug, Clone)]
pub struct Resolution {
    pub selection: Selection,
    pub alternatives: Vec<NodeId>,
}

/// Where the source view should scroll for a selection's anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTarget {
    pub path: String,
    pub file: Option<u32>,
    pub def: Option<Range>,
    pub inst: Option<Range>,
}

/// What the waveform view should show for a selection — or that it is absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaveTarget {
    Linked {
        var_ref: u32,
        full_name: String,
    },
    /// The object exists in the design but has no signal in the loaded trace.
    NotInTrace,
}

/// The cross-probe engine over a matched design + loaded trace.
pub struct CrossProbe {
    design: Design,
    var_by_ref: HashMap<u32, WaveVar>,
    ref_by_name: HashMap<String, u32>,
    context: Option<NodeId>,
    report: MatchReport,
}

impl CrossProbe {
    /// Build the engine: run the matcher to populate `wave_index`, then capture
    /// the trace's var table for name/ref lookups.
    pub fn build(mut design: Design, wave: &LoadedWave, opts: &MatchOptions) -> Self {
        let signals = wave.signals();
        let report = run_match(&mut design, &signals, opts);
        let mut var_by_ref = HashMap::with_capacity(signals.len());
        let mut ref_by_name = HashMap::with_capacity(signals.len());
        for s in signals {
            ref_by_name.insert(s.full_name.clone(), s.var_ref);
            var_by_ref.insert(s.var_ref, s);
        }
        Self {
            design,
            var_by_ref,
            ref_by_name,
            context: None,
            report,
        }
    }

    pub fn design(&self) -> &Design {
        &self.design
    }

    pub fn report(&self) -> &MatchReport {
        &self.report
    }

    pub fn set_context(&mut self, ctx: NodeId) {
        self.context = Some(ctx);
    }

    pub fn clear_context(&mut self) {
        self.context = None;
    }

    pub fn context(&self) -> Option<NodeId> {
        self.context
    }

    fn path_of(&self, id: NodeId) -> &str {
        self.design.node(id).map(|n| n.path.as_str()).unwrap_or("")
    }

    // -- source → selection -------------------------------------------------

    /// Resolve a source byte `offset` in `file` to a selection. Keeps only the
    /// innermost (narrowest-range) nodes, so enclosing scopes drop out; ties are
    /// the genuine generate ambiguity, surfaced as `alternatives`.
    pub fn from_source(&self, file: u32, offset: usize) -> Option<Resolution> {
        let cands = self.design.nodes_at_source(file, offset);
        let widths: Vec<(NodeId, usize)> = cands
            .iter()
            .filter_map(|&id| self.covering_width(id, file, offset).map(|w| (id, w)))
            .collect();
        let min_w = widths.iter().map(|&(_, w)| w).min()?;
        let inner: Vec<NodeId> = widths
            .into_iter()
            .filter(|&(_, w)| w == min_w)
            .map(|(id, _)| id)
            .collect();
        Some(self.resolve_candidates(inner))
    }

    /// Resolve a node (by canonical path) to a selection — e.g. for a click in a
    /// hierarchy/schematic view.
    pub fn from_node_path(&self, path: &str) -> Option<Resolution> {
        let ids = self.design.nodes_at_path(path);
        if ids.is_empty() {
            return None;
        }
        Some(self.resolve_candidates(ids.to_vec()))
    }

    // -- waveform → selection ----------------------------------------------

    /// Resolve a waveform variable (by wellen var-ref) to a selection.
    pub fn from_wave(&self, var_ref: u32) -> Option<Resolution> {
        let nid = self
            .design
            .wave_index
            .node_of(WaveSignalRef(var_ref as u64))?;
        Some(Resolution {
            selection: self.selection_for(nid),
            alternatives: vec![],
        })
    }

    /// Resolve a waveform signal by its full (trace) name.
    pub fn from_signal(&self, full_name: &str) -> Option<Resolution> {
        let vr = *self.ref_by_name.get(full_name)?;
        self.from_wave(vr)
    }

    // -- projections --------------------------------------------------------

    /// Where the source view scrolls for this selection's anchor.
    pub fn to_source(&self, sel: &Selection) -> SourceTarget {
        let n = self.design.node(sel.anchor);
        SourceTarget {
            path: n.map(|n| n.path.clone()).unwrap_or_default(),
            file: n.and_then(|n| n.def_range.or(n.inst_range)).map(|r| r.file),
            def: n.and_then(|n| n.def_range),
            inst: n.and_then(|n| n.inst_range),
        }
    }

    /// What the waveform view shows — the first linked signal in the equivalence
    /// set (anchor first), or `NotInTrace`.
    pub fn to_wave(&self, sel: &Selection) -> WaveTarget {
        let order = std::iter::once(sel.anchor)
            .chain(sel.nodes.iter().copied().filter(|&n| n != sel.anchor));
        for nid in order {
            if let Some(sig) = self.design.wave_index.signal_of(nid) {
                let var_ref = sig.0 as u32;
                let full_name = self
                    .var_by_ref
                    .get(&var_ref)
                    .map(|v| v.full_name.clone())
                    .unwrap_or_default();
                return WaveTarget::Linked { var_ref, full_name };
            }
        }
        WaveTarget::NotInTrace
    }

    /// The equivalence set (all nodes sharing the node's path), anchored on `nid`.
    pub fn selection_for(&self, nid: NodeId) -> Selection {
        let path = self.path_of(nid).to_string();
        let nodes = self.design.nodes_at_path(&path).to_vec();
        Selection { anchor: nid, nodes }
    }

    // -- internals ----------------------------------------------------------

    /// Width of the narrowest def/inst range that covers `offset`; degenerate
    /// zero-width points are deprioritized (treated as max).
    fn covering_width(&self, id: NodeId, file: u32, offset: usize) -> Option<usize> {
        let n = self.design.node(id)?;
        [n.def_range, n.inst_range]
            .into_iter()
            .flatten()
            .filter(|r| {
                let (lo, hi) = (r.start.offset as usize, r.end.offset as usize);
                r.file == file && lo <= offset && offset < hi.max(lo + 1)
            })
            .map(|r| {
                let w = r.end.offset.saturating_sub(r.start.offset) as usize;
                if w == 0 {
                    usize::MAX
                } else {
                    w
                }
            })
            .min()
    }

    /// Group candidates by path, pick a best-kind rep per path, choose the anchor
    /// path against `context`, and surface the other paths as `alternatives`.
    fn resolve_candidates(&self, candidates: Vec<NodeId>) -> Resolution {
        let mut by_path: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
        for id in candidates {
            by_path
                .entry(self.path_of(id).to_string())
                .or_default()
                .push(id);
        }
        // Deterministic order; rep = best signal kind at each path.
        let reps: Vec<(String, NodeId)> = by_path
            .iter()
            .map(|(p, ids)| (p.clone(), self.best_kind(ids)))
            .collect();

        let anchor_idx = self.choose_anchor(&reps);
        let anchor_path = &reps[anchor_idx].0;
        let nodes = by_path.get(anchor_path).cloned().unwrap_or_default();
        let anchor = reps[anchor_idx].1;
        let alternatives = reps
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != anchor_idx)
            .map(|(_, (_, n))| *n)
            .collect();
        Resolution {
            selection: Selection { anchor, nodes },
            alternatives,
        }
    }

    fn best_kind(&self, ids: &[NodeId]) -> NodeId {
        let rank = |id: NodeId| match self.design.node(id).map(|n| n.kind) {
            Some(NodeKind::Var) => 0,
            Some(NodeKind::Net) => 1,
            Some(NodeKind::Port) => 2,
            Some(NodeKind::Instance) => 3,
            Some(NodeKind::GenBlock) => 4,
            _ => 5,
        };
        *ids.iter().min_by_key(|&&id| rank(id)).unwrap()
    }

    /// Pick the candidate whose path is under the active context scope; else the
    /// first (deterministic).
    fn choose_anchor(&self, reps: &[(String, NodeId)]) -> usize {
        if let Some(ctx) = self.context {
            let cpath = self.path_of(ctx).to_string();
            let prefix = format!("{cpath}.");
            if let Some(i) = reps
                .iter()
                .position(|(p, _)| *p == cpath || p.starts_with(&prefix))
            {
                return i;
            }
        }
        0
    }
}
