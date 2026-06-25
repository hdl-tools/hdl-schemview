//! Phase 2 cross-probe exit gate, on the committed fixture, both FST and VCD:
//! bidirectional source↔waveform, generate ambiguity via context + picker, and
//! loud trace-misses.

use std::path::PathBuf;

use svxprobe_matcher::MatchOptions;
use svxprobe_model::NodeId;
use svxprobe_xprobe::{CrossProbe, WaveTarget};

fn fixture(rel: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/picorv32_soc")
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

fn build(ext: &str) -> CrossProbe {
    let design = svxprobe_ingest::from_path(fixture("golden/hierarchy.json")).unwrap();
    let wave =
        svxprobe_wave::LoadedWave::open(&fixture(&format!("traces/picorv32_soc.{ext}"))).unwrap();
    let opts = MatchOptions {
        excluded_scopes: vec!["TOP".into(), "tb".into(), "soc_pkg".into()],
        anchor: None,
    };
    CrossProbe::build(design, &wave, &opts)
}

fn path_of(cp: &CrossProbe, id: NodeId) -> String {
    cp.design().node(id).unwrap().path.clone()
}

/// waveform → source → waveform, for a known interface signal.
fn round_trip(ext: &str) {
    let cp = build(ext);
    let signal = "TOP.tb.dut.g_lane[0].bus.valid";

    // waveform → selection
    let res = cp.from_signal(signal).expect("signal resolves");
    assert_eq!(
        path_of(&cp, res.selection.anchor),
        "picorv32_soc.g_lane[0].bus.valid",
        "[{ext}] anchor path"
    );

    // selection → source target
    let src = cp.to_source(&res.selection);
    let def = src.def.expect("has a def range");

    // source → selection (must include the same canonical path)
    let back = cp
        .from_source(def.file, def.start.offset as usize)
        .expect("source resolves");
    let mut all = back.alternatives.clone();
    all.push(back.selection.anchor);
    assert!(
        all.iter()
            .any(|&n| path_of(&cp, n) == "picorv32_soc.g_lane[0].bus.valid"),
        "[{ext}] round-trip lost the node"
    );

    // selection → waveform. A node can be linked by several trace vars (the
    // direct signal and its interface-modport alias), so assert round-trip
    // *identity* (the returned var resolves back to the same node) rather than an
    // exact name, which is the meaningful cross-probe invariant.
    match cp.to_wave(&res.selection) {
        WaveTarget::Linked { var_ref, .. } => {
            let back = cp.from_wave(var_ref).expect("var resolves back");
            assert_eq!(
                path_of(&cp, back.selection.anchor),
                path_of(&cp, res.selection.anchor),
                "[{ext}] wave→node→wave→node round-trip changed node"
            );
        }
        WaveTarget::NotInTrace => panic!("[{ext}] bus.valid should be in trace"),
    }
}

/// A source position inside the generate body is ambiguous across lanes and is
/// disambiguated by context, with the other lane offered in the picker.
fn picker(ext: &str) {
    let mut cp = build(ext);

    // lane_state is declared once but unrolled into g_lane[0] and g_lane[1].
    let l0 = cp
        .design()
        .nodes_at_path("picorv32_soc.g_lane[0].lane_state")[0];
    let def = cp.design().node(l0).unwrap().def_range.unwrap();

    // No context: one anchor + exactly one alternative (the other lane).
    let res = cp
        .from_source(def.file, def.start.offset as usize)
        .expect("resolves");
    assert_eq!(
        res.alternatives.len(),
        1,
        "[{ext}] expected one picker alternative"
    );
    let mut paths: Vec<String> = res.alternatives.iter().map(|&n| path_of(&cp, n)).collect();
    paths.push(path_of(&cp, res.selection.anchor));
    paths.sort();
    assert_eq!(
        paths,
        vec![
            "picorv32_soc.g_lane[0].lane_state".to_string(),
            "picorv32_soc.g_lane[1].lane_state".to_string(),
        ],
        "[{ext}] both lanes present"
    );

    // With context = lane 1, the anchor resolves to lane 1.
    let ctx = cp.design().nodes_at_path("picorv32_soc.g_lane[1]")[0];
    cp.set_context(ctx);
    let res = cp
        .from_source(def.file, def.start.offset as usize)
        .expect("resolves");
    assert_eq!(
        path_of(&cp, res.selection.anchor),
        "picorv32_soc.g_lane[1].lane_state",
        "[{ext}] context did not steer the anchor"
    );
}

/// A structural node (an instance) has no waveform signal — surfaced loudly.
fn not_in_trace(ext: &str) {
    let cp = build(ext);
    let core = cp.design().nodes_at_path("picorv32_soc.g_lane[0].core")[0];
    let sel = cp.selection_for(core);
    assert_eq!(
        cp.to_wave(&sel),
        WaveTarget::NotInTrace,
        "[{ext}] instance is not a signal"
    );
}

#[test]
fn round_trip_fst() {
    round_trip("fst");
}
#[test]
fn round_trip_vcd() {
    round_trip("vcd");
}
#[test]
fn picker_fst() {
    picker("fst");
}
#[test]
fn picker_vcd() {
    picker("vcd");
}
#[test]
fn not_in_trace_fst() {
    not_in_trace("fst");
}
#[test]
fn not_in_trace_vcd() {
    not_in_trace("vcd");
}
