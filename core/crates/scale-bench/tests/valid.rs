//! Correctness guardrail: the synthetic generator must produce a document that
//! loads through the *real* ingest path and satisfies the invariants the benches
//! rely on. Runs under the existing `cargo test --all` PR gate (fast, small size).

use scale_bench::{build_design, generate, synth_signals, to_json, SynthConfig};
use svxprobe_matcher::{run_match, MatchOptions};
use svxprobe_model::NodeKind;

/// A small config that fills every per-leaf slot yet stays sub-second.
fn small() -> SynthConfig {
    SynthConfig {
        node_count: 2_000,
        fanout: 4,
        depth: 3,
        signal_count: 1_000,
        seed: 0xC0FF_EE00,
    }
}

#[test]
fn generator_output_passes_validate() {
    let synth = generate(&small());
    // The headline invariant: the eager ingest path (parse + validate + index)
    // accepts the synthetic document.
    let json = to_json(&synth.doc);
    assert!(
        svxprobe_ingest::from_slice(&json).is_ok(),
        "synthetic document must pass ingest::validate"
    );
}

#[test]
fn node_ids_are_indices() {
    let synth = generate(&small());
    for (i, n) in synth.doc.nodes.iter().enumerate() {
        assert_eq!(n.id as usize, i, "node.id must equal its index");
    }
}

#[test]
fn clock_net_has_expected_fanout() {
    let (design, synth) = build_design(&small());
    assert!(synth.clock_fanout > 0, "clock must fan out to some FFs");
    assert_eq!(
        design.edges_of(synth.clock_net).len(),
        synth.clock_fanout,
        "clock_net degree matches recorded fanout"
    );
}

#[test]
fn mid_scope_scope_graph_nonempty() {
    let (design, synth) = build_design(&small());
    let g = svxprobe_schematic::scope_graph(&design, &synth.mid_scope)
        .expect("mid scope resolves to a graph");
    assert!(!g.nodes.is_empty(), "mid scope has at least one box");
}

#[test]
fn probe_handles_resolve() {
    let (design, synth) = build_design(&small());
    assert!(
        !design.nodes_at_path(&synth.probe_path).is_empty(),
        "probe_path resolves to a node"
    );
    let (file, off) = synth.probe_source;
    assert!(
        !design.nodes_at_source(file, off).is_empty(),
        "probe_source lands inside a node's range"
    );
}

#[test]
fn synth_signals_match() {
    let (mut design, synth) = build_design(&small());
    let signals = synth_signals(&synth, &design, small().signal_count);
    assert!(!signals.is_empty());
    let opts = MatchOptions {
        excluded_scopes: vec![],
        anchor: Some(vec!["top".to_string()]),
    };
    let report = run_match(&mut design, &signals, &opts);
    assert!(
        report.hit_rate() > 0.99,
        "synthetic signals should match near-fully, got {}",
        report.hit_rate()
    );
    assert_eq!(report.mystery, 0, "no mystery misses");
}

#[test]
fn generation_is_deterministic() {
    let a = to_json(&generate(&SynthConfig::mid()).doc);
    let b = to_json(&generate(&SynthConfig::mid()).doc);
    assert_eq!(a, b, "same seed ⇒ byte-identical output");
}

#[test]
fn generator_emits_port_nodes() {
    // The dual-node Port+Net pairs share a name within a scope and must still
    // pass validate (checked above) — confirm ports are actually emitted.
    let synth = generate(&small());
    let ports = synth
        .doc
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Port)
        .count();
    assert!(ports > 0, "generator emits port nodes");
}
