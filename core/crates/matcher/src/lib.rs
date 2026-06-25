//! The canonical-path matcher (roadmap Phase 1 — the go/no-go).
//!
//! Maps every waveform signal to a NodeId by normalizing its scope path and
//! looking it up in the design's `path_index`, then reports a hit-rate. Every
//! miss is attributed to a named reason; the gate requires the hit-rate to clear
//! a threshold with zero "mystery" (uncategorized) misses.

mod anchor;
pub mod normalize;

use std::collections::BTreeMap;

use svxprobe_model::{Design, NodeId, NodeKind, WaveSignalRef};
use svxprobe_wave::WaveVar;

pub use anchor::detect_anchor;
pub use normalize::{classify, Classification, NormalizeConfig, Rule};

/// Options controlling a match run.
#[derive(Debug, Clone, Default)]
pub struct MatchOptions {
    /// Scope segment names marking out-of-design signals as excluded.
    pub excluded_scopes: Vec<String>,
    /// Force the DUT anchor instead of auto-detecting it.
    pub anchor: Option<Vec<String>>,
}

/// A signal that could not be matched, with the candidate path that was tried.
#[derive(Debug, Clone)]
pub struct UnmatchedSignal {
    pub full_name: String,
    pub candidate: String,
}

/// Result of a match run.
#[derive(Debug, Clone)]
pub struct MatchReport {
    pub total_signals: usize,
    /// In-design, non-parameter signals — the denominator for the gate.
    pub denominator: usize,
    pub matched_direct: usize,
    pub matched_alias: usize,
    pub unmatched: usize,
    pub excluded_scope: usize,
    pub excluded_parameter: usize,
    /// Out-of-design and not in any excluded scope — fails the gate if > 0.
    pub mystery: usize,
    pub anchor: Vec<String>,
    pub rule_counts: BTreeMap<Rule, usize>,
    pub unmatched_samples: Vec<UnmatchedSignal>,
    pub mystery_samples: Vec<String>,
}

impl MatchReport {
    pub fn matched(&self) -> usize {
        self.matched_direct + self.matched_alias
    }

    /// Fraction of design-scope signals matched (1.0 if the denominator is 0).
    pub fn hit_rate(&self) -> f64 {
        if self.denominator == 0 {
            1.0
        } else {
            self.matched() as f64 / self.denominator as f64
        }
    }

    /// The Phase 1 gate: hit-rate clears `threshold` AND no mystery misses.
    pub fn passes_gate(&self, threshold: f64) -> bool {
        self.hit_rate() >= threshold && self.mystery == 0
    }
}

/// Run the matcher, populating `design.wave_index` for matched signals.
pub fn run_match(design: &mut Design, signals: &[WaveVar], opts: &MatchOptions) -> MatchReport {
    let design_top = design.doc.design.clone();
    let anchor = opts
        .anchor
        .clone()
        .unwrap_or_else(|| detect_anchor(design, signals, &design_top));
    let cfg = NormalizeConfig {
        design_top,
        anchor: anchor.clone(),
        excluded: opts.excluded_scopes.clone(),
    };

    let mut report = MatchReport {
        total_signals: signals.len(),
        denominator: 0,
        matched_direct: 0,
        matched_alias: 0,
        unmatched: 0,
        excluded_scope: 0,
        excluded_parameter: 0,
        mystery: 0,
        anchor,
        rule_counts: BTreeMap::new(),
        unmatched_samples: Vec::new(),
        mystery_samples: Vec::new(),
    };
    let mut to_index: Vec<(NodeId, u32)> = Vec::new();

    for s in signals {
        match classify(&cfg, &s.full_name) {
            Classification::InDesign {
                candidate,
                mut trail,
            } => {
                // Resolve via the interface-alias fallback (drop a holder
                // segment) when the direct path has no node.
                let alias = alias_path(design, &candidate);
                let direct_signal = pick_node(design, &candidate);
                let alias_signal = alias.as_deref().and_then(|p| pick_node(design, p));

                if let Some(nid) = direct_signal {
                    report.matched_direct += 1;
                    record_rules(&mut report.rule_counts, &trail);
                    to_index.push((nid, s.var_ref));
                } else if let Some(nid) = alias_signal {
                    trail.push(Rule::InterfaceAlias);
                    report.matched_alias += 1;
                    record_rules(&mut report.rule_counts, &trail);
                    to_index.push((nid, s.var_ref));
                } else if s.is_parameter
                    || is_parameter_path(design, &candidate)
                    || alias
                        .as_deref()
                        .is_some_and(|p| is_parameter_path(design, p))
                {
                    // A simulator-dumped parameter, not a real signal (possibly
                    // viewed through an interface modport). The golden is
                    // authoritative; the trace's own flag is a fallback.
                    report.excluded_parameter += 1;
                } else {
                    report.unmatched += 1;
                    if report.unmatched_samples.len() < 100 {
                        report.unmatched_samples.push(UnmatchedSignal {
                            full_name: s.full_name.clone(),
                            candidate,
                        });
                    }
                }
            }
            Classification::Excluded => report.excluded_scope += 1,
            Classification::OutOfDesign => {
                report.mystery += 1;
                if report.mystery_samples.len() < 100 {
                    report.mystery_samples.push(s.full_name.clone());
                }
            }
        }
    }

    // Denominator = in-design signals that are not parameters.
    report.denominator = report.matched() + report.unmatched;

    for (nid, var_ref) in to_index {
        design.wave_index.insert(nid, WaveSignalRef(var_ref as u64));
    }
    report
}

/// Pick the best *signal* node at a path: prefer Var > Net > Port. Returns
/// `None` if the path has no signal node (e.g. only a Param/Instance/GenBlock).
fn pick_node(design: &Design, path: &str) -> Option<NodeId> {
    let rank = |id: NodeId| match design.node(id).map(|n| n.kind) {
        Some(NodeKind::Var) => 0,
        Some(NodeKind::Net) => 1,
        Some(NodeKind::Port) => 2,
        _ => 99,
    };
    design
        .nodes_at_path(path)
        .iter()
        .copied()
        .map(|id| (rank(id), id))
        .filter(|&(r, _)| r < 99)
        .min()
        .map(|(_, id)| id)
}

/// True if `path` resolves to a parameter node (and no signal node).
fn is_parameter_path(design: &Design, path: &str) -> bool {
    let ids = design.nodes_at_path(path);
    !ids.is_empty()
        && ids
            .iter()
            .all(|&id| design.node(id).map(|n| n.kind) == Some(NodeKind::Param))
}

/// Interface-alias fallback: an interface modport-port view (e.g.
/// `…memory.bus.clk`) aliases the interface instance (`…bus.clk`). Try dropping
/// each interior segment; return the path only if exactly one variant resolves
/// to a node (signal or parameter). Refuses to guess when ambiguous.
fn alias_path(design: &Design, candidate: &str) -> Option<String> {
    let segs: Vec<&str> = candidate.split('.').collect();
    if segs.len() < 3 {
        return None;
    }
    let mut hit: Option<String> = None;
    for drop in 1..segs.len() - 1 {
        let mut alt = Vec::with_capacity(segs.len() - 1);
        alt.extend_from_slice(&segs[..drop]);
        alt.extend_from_slice(&segs[drop + 1..]);
        let path = alt.join(".");
        if !design.nodes_at_path(&path).is_empty() {
            if hit.is_some() {
                return None; // ambiguous — refuse to guess
            }
            hit = Some(path);
        }
    }
    hit
}

fn record_rules(counts: &mut BTreeMap<Rule, usize>, trail: &[Rule]) {
    for &r in trail {
        *counts.entry(r).or_default() += 1;
    }
}
