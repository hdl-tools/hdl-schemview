//! The canonical-path normalizer.
//!
//! Transforms a waveform signal's full hierarchical name (as wellen reports it,
//! e.g. `TOP.tb.dut.g_lane[0].core.cpuregs.[5]`) into a candidate canonical path
//! in the elaborated design's namespace (`picorv32_soc.g_lane[0].core.cpuregs`),
//! or classifies it as out-of-design. Every transformation is a named rule so
//! misses can be attributed (the gate forbids "mystery" misses).

/// Named normalization rules, recorded in a match's trail for auditability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    /// Stripped the sim-top + testbench anchor prefix and rebased to the design top.
    AnchorRebase,
    /// Dropped a pure array-element segment, e.g. `.[5]` (the array is one node).
    ArrayElement,
    /// Dropped an unnamed generate segment, e.g. `genblk1` (sim flattens it).
    GenBlockFlatten,
    /// Resolved an interface modport-port view to the interface instance by
    /// dropping the holding-instance segment (see the matcher's alias fallback).
    InterfaceAlias,
}

/// How a trace signal relates to the design namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// Under the design anchor; `candidate` is the canonical path to look up.
    InDesign { candidate: String, trail: Vec<Rule> },
    /// Outside the design but in a known excluded scope (TB/sim/package).
    Excluded,
    /// Outside the design and NOT in any excluded scope — a mystery (fails gate).
    OutOfDesign,
}

/// Configuration for [`classify`].
#[derive(Debug, Clone)]
pub struct NormalizeConfig {
    /// The design top module name (golden namespace root), e.g. `picorv32_soc`.
    pub design_top: String,
    /// Trace scope segments identifying the DUT, e.g. `["TOP","tb","dut"]`.
    pub anchor: Vec<String>,
    /// Scope segment names that mark an out-of-design signal as excluded.
    pub excluded: Vec<String>,
}

/// True for a segment that is a pure array-element selector like `[5]`.
fn is_array_element(seg: &str) -> bool {
    seg.len() > 2
        && seg.starts_with('[')
        && seg.ends_with(']')
        && seg[1..seg.len() - 1].chars().all(|c| c.is_ascii_digit())
}

/// True for an unnamed generate segment like `genblk1` (digits after `genblk`).
fn is_genblock(seg: &str) -> bool {
    seg.len() > "genblk".len()
        && seg.starts_with("genblk")
        && seg["genblk".len()..].chars().all(|c| c.is_ascii_digit())
}

fn starts_with_anchor(segs: &[&str], anchor: &[String]) -> bool {
    segs.len() >= anchor.len() && anchor.iter().zip(segs).all(|(a, s)| a == s)
}

/// Normalize/classify a trace full-name against the design namespace.
pub fn classify(cfg: &NormalizeConfig, full_name: &str) -> Classification {
    let segs: Vec<&str> = full_name.split('.').collect();

    if starts_with_anchor(&segs, &cfg.anchor) {
        let mut out = vec![cfg.design_top.clone()];
        let mut trail = vec![Rule::AnchorRebase];
        for &seg in &segs[cfg.anchor.len()..] {
            if is_array_element(seg) {
                push_once(&mut trail, Rule::ArrayElement);
                continue;
            }
            if is_genblock(seg) {
                push_once(&mut trail, Rule::GenBlockFlatten);
                continue;
            }
            out.push(seg.to_string());
        }
        return Classification::InDesign {
            candidate: out.join("."),
            trail,
        };
    }

    if segs.iter().any(|s| cfg.excluded.iter().any(|e| e == s)) {
        Classification::Excluded
    } else {
        Classification::OutOfDesign
    }
}

fn push_once(trail: &mut Vec<Rule>, rule: Rule) {
    if !trail.contains(&rule) {
        trail.push(rule);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> NormalizeConfig {
        NormalizeConfig {
            design_top: "picorv32_soc".into(),
            anchor: vec!["TOP".into(), "tb".into(), "dut".into()],
            excluded: vec!["TOP".into(), "tb".into(), "soc_pkg".into()],
        }
    }

    fn candidate(c: Classification) -> String {
        match c {
            Classification::InDesign { candidate, .. } => candidate,
            other => panic!("expected InDesign, got {other:?}"),
        }
    }

    #[test]
    fn anchor_rebase_plain() {
        let c = classify(&cfg(), "TOP.tb.dut.g_lane[0].bus.valid");
        assert_eq!(candidate(c), "picorv32_soc.g_lane[0].bus.valid");
    }

    #[test]
    fn generate_array_index_is_preserved() {
        // g_lane[0] is a fused segment (named generate), NOT a bare [0].
        let c = classify(&cfg(), "TOP.tb.dut.g_lane[1].core.trap");
        assert_eq!(candidate(c), "picorv32_soc.g_lane[1].core.trap");
    }

    #[test]
    fn array_element_segment_is_dropped() {
        let c = classify(&cfg(), "TOP.tb.dut.g_lane[0].core.cpuregs.[5]");
        match c {
            Classification::InDesign { candidate, trail } => {
                assert_eq!(candidate, "picorv32_soc.g_lane[0].core.cpuregs");
                assert!(trail.contains(&Rule::ArrayElement));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn genblock_is_flattened() {
        let c = classify(&cfg(), "TOP.tb.dut.g_lane[0].core.genblk1.foo");
        match c {
            Classification::InDesign { candidate, trail } => {
                assert_eq!(candidate, "picorv32_soc.g_lane[0].core.foo");
                assert!(trail.contains(&Rule::GenBlockFlatten));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn excluded_vs_mystery() {
        assert_eq!(classify(&cfg(), "TOP.tb.N_CORES"), Classification::Excluded);
        assert_eq!(
            classify(&cfg(), "TOP.soc_pkg.XLEN"),
            Classification::Excluded
        );
        // A scope under no excluded name and not the anchor is a mystery.
        let mystery = classify(
            &NormalizeConfig {
                excluded: vec![],
                ..cfg()
            },
            "OTHER.weird.sig",
        );
        assert_eq!(mystery, Classification::OutOfDesign);
    }
}
