//! Anchor auto-detection.
//!
//! The trace names the DUT by its *instance* name (e.g. `dut`), not the design
//! module name (`picorv32_soc`), and wraps it in a testbench + simulator top
//! (`TOP.tb.dut`). We can't hard-code that prefix, so we detect it: try every
//! candidate scope prefix and pick the one that, when rebased to the design top,
//! makes the most signals resolve to a real canonical path.

use svxprobe_model::Design;
use svxprobe_wave::WaveVar;

/// Maximum anchor depth to consider (TB wrappers are rarely deeper).
const MAX_ANCHOR_DEPTH: usize = 6;

/// Detect the DUT anchor (leading trace scope segments) maximizing matches.
///
/// Returns the segments to strip, e.g. `["TOP","tb","dut"]`. Falls back to an
/// empty anchor (design at trace root) if nothing scores better.
pub fn detect_anchor(design: &Design, signals: &[WaveVar], design_top: &str) -> Vec<String> {
    // Collect candidate prefixes (segment vectors) from the signal scope paths.
    let mut candidates: Vec<Vec<String>> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in signals {
        let segs: Vec<&str> = s.full_name.split('.').collect();
        // A signal needs at least one segment beyond the anchor (the var name).
        let max = segs.len().saturating_sub(1).min(MAX_ANCHOR_DEPTH);
        for depth in 0..=max {
            let prefix: Vec<String> = segs[..depth].iter().map(|s| s.to_string()).collect();
            if seen.insert(prefix.clone()) {
                candidates.push(prefix);
            }
        }
    }

    let mut best: Vec<String> = Vec::new();
    let mut best_score = 0usize;
    for prefix in candidates {
        let score = score_prefix(design, signals, design_top, &prefix);
        // Prefer a higher score; tie-break on the longer (more specific) prefix.
        if score > 0 && (score > best_score || (score == best_score && prefix.len() > best.len())) {
            best = prefix;
            best_score = score;
        }
    }
    best
}

/// How many signals resolve to a real canonical path under this anchor prefix
/// (direct path lookup only — the alias fallback is applied later).
fn score_prefix(
    design: &Design,
    signals: &[WaveVar],
    design_top: &str,
    prefix: &[String],
) -> usize {
    let mut hits = 0;
    for s in signals {
        if s.is_parameter {
            continue;
        }
        let segs: Vec<&str> = s.full_name.split('.').collect();
        if segs.len() <= prefix.len() || prefix.iter().zip(&segs).any(|(p, x)| p != x) {
            continue;
        }
        let mut path = String::from(design_top);
        for &seg in &segs[prefix.len()..] {
            // Skip pure array-element segments so the array node still matches.
            if seg.starts_with('[') && seg.ends_with(']') {
                continue;
            }
            path.push('.');
            path.push_str(seg);
        }
        if !design.nodes_at_path(&path).is_empty() {
            hits += 1;
        }
    }
    hits
}
