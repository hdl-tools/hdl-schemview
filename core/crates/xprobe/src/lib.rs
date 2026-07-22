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
use std::hash::{Hash, Hasher};
use std::path::Path;

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

/// A hash of the match options that affect the resulting `wave_index` (excluded
/// scopes + forced anchor). Keys the wave_index cache so a different exclusion set
/// or anchor is never served a stale mapping (#153). Stability across toolchains
/// isn't required — a hash change simply forces a (safe) cache miss + re-match.
pub fn match_opts_hash(opts: &MatchOptions) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    opts.excluded_scopes.hash(&mut h);
    opts.anchor.hash(&mut h);
    h.finish()
}

/// The cross-probe engine over a matched design + loaded trace.
pub struct CrossProbe {
    design: Design,
    var_by_ref: HashMap<u32, WaveVar>,
    ref_by_name: HashMap<String, u32>,
    context: Option<NodeId>,
    /// The match run's diagnostics — `None` when the `wave_index` was restored
    /// from the persisted cache instead of matched (#153), which carries no report.
    report: Option<MatchReport>,
}

impl CrossProbe {
    /// Build the engine: run the matcher to populate `wave_index`, then capture
    /// the trace's var table for name/ref lookups.
    pub fn build(mut design: Design, wave: &LoadedWave, opts: &MatchOptions) -> Self {
        let signals = wave.signals();
        let report = run_match(&mut design, &signals, opts);
        Self::assemble(design, wave, Some(report))
    }

    /// Like [`build`](Self::build), but reuses a persisted `wave_index` when a
    /// fresh cache exists for `(model, trace, opts)` — skipping the ~O(signals ×
    /// path_len) matcher pass, the dominant per-launch cost once the model parse
    /// is cached (#21). On a miss it matches as normal and best-effort persists
    /// the result. `model`/`trace` are the on-disk paths that key the cache.
    pub fn build_cached(
        mut design: Design,
        wave: &LoadedWave,
        opts: &MatchOptions,
        model: &Path,
        trace: &Path,
    ) -> Self {
        let opts_hash = match_opts_hash(opts);
        if let Some(pairs) = svxprobe_ingest::try_load_wave_index(model, trace, opts_hash) {
            design.wave_index = svxprobe_model::WaveIndex::from_pairs(
                pairs.into_iter().map(|(n, s)| (n, WaveSignalRef(s))),
            );
            return Self::assemble(design, wave, None);
        }
        // Miss: run the matcher, then persist the resolved pairs (best-effort — a
        // cache-write failure must not fail the load).
        let signals = wave.signals();
        let report = run_match(&mut design, &signals, opts);
        let pairs: Vec<(u32, u64)> = design.wave_index.pairs().map(|(n, s)| (n, s.0)).collect();
        let _ = svxprobe_ingest::write_wave_index(model, trace, opts_hash, &pairs);
        Self::assemble(design, wave, Some(report))
    }

    /// Re-match the (unchanged) design against a different trace, in place (#176).
    /// The design is the invariant — only the trace and its matching change — so a
    /// waveform pane's "Load trace…" (#170) can swap traces without re-ingesting the
    /// model or re-running the elaboration harness.
    ///
    /// The previous trace's `wave_index` is dropped first: [`run_match`] only ever
    /// *inserts*, so a leftover mapping would keep resolving nodes to signal refs of
    /// the old trace — refs the new one may not have, or may have given to a
    /// different signal.
    pub fn rematch(&mut self, wave: &LoadedWave, opts: &MatchOptions) {
        self.design.wave_index = svxprobe_model::WaveIndex::default();
        let signals = wave.signals();
        let report = run_match(&mut self.design, &signals, opts);
        self.reindex(wave);
        self.report = Some(report);
        // `context` is a NodeId into the (unchanged) design, so it stays valid.
    }

    /// Like [`rematch`](Self::rematch), but reuses a persisted `wave_index` when a
    /// fresh cache exists for `(model, trace, opts)`, and persists the result on a
    /// miss (#153) — so swapping back and forth between traces pays the matcher only
    /// once per trace. `model`/`trace` are the on-disk paths that key the cache.
    pub fn rematch_cached(
        &mut self,
        wave: &LoadedWave,
        opts: &MatchOptions,
        model: &Path,
        trace: &Path,
    ) {
        let opts_hash = match_opts_hash(opts);
        if let Some(pairs) = svxprobe_ingest::try_load_wave_index(model, trace, opts_hash) {
            self.design.wave_index = svxprobe_model::WaveIndex::from_pairs(
                pairs.into_iter().map(|(n, s)| (n, WaveSignalRef(s))),
            );
            self.reindex(wave);
            self.report = None; // restored from cache — no match report
            return;
        }
        // Miss: match, then persist the resolved pairs (best-effort — a cache-write
        // failure must not fail the swap).
        self.rematch(wave, opts);
        let pairs: Vec<(u32, u64)> = self
            .design
            .wave_index
            .pairs()
            .map(|(n, s)| (n, s.0))
            .collect();
        let _ = svxprobe_ingest::write_wave_index(model, trace, opts_hash, &pairs);
    }

    /// Point the trace-side lookups at `wave`'s var table, dropping the previous
    /// trace's.
    fn reindex(&mut self, wave: &LoadedWave) {
        let (var_by_ref, ref_by_name) = Self::var_tables(wave);
        self.var_by_ref = var_by_ref;
        self.ref_by_name = ref_by_name;
    }

    /// Capture the trace's var table (name/ref lookups) around an already-populated
    /// `wave_index`. Shared by [`build`](Self::build) and
    /// [`build_cached`](Self::build_cached).
    fn assemble(design: Design, wave: &LoadedWave, report: Option<MatchReport>) -> Self {
        let (var_by_ref, ref_by_name) = Self::var_tables(wave);
        Self {
            design,
            var_by_ref,
            ref_by_name,
            context: None,
            report,
        }
    }

    /// The trace's var lookup tables: var-ref → var, and full name → var-ref.
    fn var_tables(wave: &LoadedWave) -> (HashMap<u32, WaveVar>, HashMap<String, u32>) {
        let signals = wave.signals();
        let mut var_by_ref = HashMap::with_capacity(signals.len());
        let mut ref_by_name = HashMap::with_capacity(signals.len());
        for s in signals {
            ref_by_name.insert(s.full_name.clone(), s.var_ref);
            var_by_ref.insert(s.var_ref, s);
        }
        (var_by_ref, ref_by_name)
    }

    pub fn design(&self) -> &Design {
        &self.design
    }

    /// The match run's diagnostics, or `None` if the `wave_index` was restored
    /// from cache (#153).
    pub fn report(&self) -> Option<&MatchReport> {
        self.report.as_ref()
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

    /// The trace variable for a wellen var-ref (name + signal-ref for loading).
    pub fn wave_var(&self, var_ref: u32) -> Option<&WaveVar> {
        self.var_by_ref.get(&var_ref)
    }

    fn path_of(&self, id: NodeId) -> &str {
        self.design.node(id).map(|n| n.path.as_str()).unwrap_or("")
    }

    // -- source → selection -------------------------------------------------

    /// Resolve a source byte `offset` in `file` to a selection. Keeps only the
    /// innermost (narrowest-range) nodes, so enclosing scopes drop out; ties are
    /// the genuine generate ambiguity, surfaced as `alternatives`.
    pub fn from_source(&self, file: u32, offset: usize) -> Option<Resolution> {
        // HLS C↔RTL tracing (#159): a click in a C/C++ source has no node of its own —
        // redirect through the provenance map to the generated-RTL span, then resolve
        // the node(s) that span *contains*, so a C click yields a normal node-anchored
        // selection (and the C pane inherits the schematic/waveform cross-probe for
        // free). A provenance span covers a whole RTL line, so resolve over the range
        // (a point at the line start would only ever land on the enclosing module).
        // Only fires when a provenance entry covers the offset, so RTL clicks are
        // unaffected.
        if let Some(rtl) = self.design.mapped_from_src(file, offset) {
            let (rtl_file, lo, hi) = (rtl.file, rtl.start.offset as usize, rtl.end.offset as usize);
            return self.resolve_source_range(rtl_file, lo, hi);
        }
        self.resolve_source_range(file, offset, offset + 1)
    }

    /// Resolve the innermost node(s) whose def/inst range overlaps `[lo, hi)` in `file`.
    /// Generalizes the point resolution so a mapped provenance line-region (#159) picks
    /// the narrowest node it contains, not just the enclosing scope. A one-byte span
    /// reproduces the exact point behaviour.
    fn resolve_source_range(&self, file: u32, lo: usize, hi: usize) -> Option<Resolution> {
        let cands = self.design.nodes_in_source_range(file, lo, hi);
        let widths: Vec<(NodeId, usize)> = cands
            .iter()
            .filter_map(|&id| self.overlapping_width(id, file, lo, hi).map(|w| (id, w)))
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

    /// Width of the narrowest def/inst range that overlaps `[lo, hi)`; degenerate
    /// zero-width points are deprioritized (treated as max). A one-byte span
    /// `[off, off+1)` reproduces the old point-covering behaviour exactly.
    fn overlapping_width(&self, id: NodeId, file: u32, lo: usize, hi: usize) -> Option<usize> {
        let n = self.design.node(id)?;
        [n.def_range, n.inst_range]
            .into_iter()
            .flatten()
            .filter(|r| {
                let (r_lo, r_hi) = (r.start.offset as usize, r.end.offset as usize);
                r.file == file && r_lo < hi && lo < r_hi.max(r_lo + 1)
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
        // The full path-equivalence set (as in `selection_for`), not just the
        // candidates that led here: a source click can land on a *view* node —
        // e.g. a modport pin's declaration — and the wave/source links live on
        // its same-path siblings (the member Var), which `to_wave` walks.
        let nodes = self.design.nodes_at_path(anchor_path).to_vec();
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
