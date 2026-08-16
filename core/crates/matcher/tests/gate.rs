//! The Phase 1 go/no-go gate, run against the committed tier-1 fixture on BOTH
//! the FST and the VCD. Mirrors `svxprobe match ... --excluded excluded_scopes.txt`.

use std::path::PathBuf;

use svxprobe_matcher::{run_match, MatchOptions, Rule};

fn fixture(rel: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/picorv32_soc")
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

fn opts() -> MatchOptions {
    // The fixture's excluded_scopes.txt names: TOP (sim top), tb (testbench),
    // soc_pkg (package). Kept inline so the test is self-contained. TOP is still
    // listed although Verilator 5.040 no longer emits it (#280) — the file keeps
    // the entry for older Verilators, and an exclusion matching nothing is inert.
    MatchOptions {
        excluded_scopes: vec!["TOP".into(), "tb".into(), "soc_pkg".into()],
        anchor: None, // exercise auto-detection
    }
}

fn run(ext: &str) -> (svxprobe_model::Design, svxprobe_matcher::MatchReport) {
    let mut design = svxprobe_ingest::from_path(fixture("golden/hierarchy.json")).unwrap();
    let wave =
        svxprobe_wave::LoadedWave::open(&fixture(&format!("traces/picorv32_soc.{ext}"))).unwrap();
    let report = run_match(&mut design, &wave.signals(), &opts());
    (design, report)
}

fn assert_gate(ext: &str) {
    let (design, r) = run(ext);

    // The pinned Phase 1 gate.
    assert!(
        r.passes_gate(0.95),
        "[{ext}] gate failed: hit_rate={:.4} mystery={} unmatched={:?}",
        r.hit_rate(),
        r.mystery,
        r.unmatched_samples,
    );
    assert_eq!(r.mystery, 0, "[{ext}] mystery misses must be zero");

    // Anchor auto-detected to the DUT instance. No leading "TOP": Verilator 5.040
    // stopped emitting the synthetic top wrapper scope that 5.034 wrapped the
    // design in, so the trace hierarchy now starts at `tb` (#280).
    assert_eq!(r.anchor, vec!["tb", "dut"], "[{ext}] anchor");

    // Deterministic counts on the frozen fixture (format-independent).
    assert_eq!(r.denominator, 1577, "[{ext}] denominator");
    assert_eq!(r.matched(), 1577, "[{ext}] matched");
    assert_eq!(r.unmatched, 0, "[{ext}] unmatched");
    assert_eq!(r.excluded_parameter, 98, "[{ext}] excluded parameters");

    // The hard cases the fixture exists to stress are exercised:
    // generate-array element collapsing + interface modport aliasing.
    assert!(
        *r.rule_counts.get(&Rule::ArrayElement).unwrap_or(&0) > 0,
        "[{ext}] array-element rule not exercised"
    );
    assert_eq!(r.matched_alias, 16, "[{ext}] interface-alias matches");

    // wave_index is populated; a specific interface signal is linked.
    assert!(!design.wave_index.is_empty(), "[{ext}] wave_index empty");
    let bus_valid = design.nodes_at_path("picorv32_soc.g_lane[0].bus.valid")[0];
    assert!(
        design.wave_index.signal_of(bus_valid).is_some(),
        "[{ext}] bus.valid not linked to a signal"
    );
}

#[test]
fn gate_fst() {
    assert_gate("fst");
}

#[test]
fn gate_vcd() {
    assert_gate("vcd");
}
