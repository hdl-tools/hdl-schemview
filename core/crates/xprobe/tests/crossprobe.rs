//! Phase 2 cross-probe exit gate, on the committed fixture, both FST and VCD:
//! bidirectional source↔waveform, generate ambiguity via context + picker, and
//! loud trace-misses.

use std::path::{Path, PathBuf};

use svxprobe_matcher::MatchOptions;
use svxprobe_model::{NameClass, NodeId, NodeKind, WaveSignalRef};
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

/// `build_cached` must consult the persisted wave_index instead of re-matching.
/// Pre-seed a fabricated cache holding a single, deliberately-wrong pair: a real
/// match populates hundreds of entries, so a resulting `len() == 1` proves the
/// hit path (deserialize + `from_pairs`) was taken (#153).
#[test]
fn build_cached_uses_persisted_wave_index() {
    let model = fixture("golden/hierarchy.json");
    let trace = fixture("traces/picorv32_soc.vcd");
    let opts = MatchOptions {
        excluded_scopes: vec!["TOP".into(), "tb".into(), "soc_pkg".into()],
        anchor: None,
    };

    let opts_hash = svxprobe_xprobe::match_opts_hash(&opts);
    svxprobe_ingest::write_wave_index(Path::new(&model), Path::new(&trace), opts_hash, &[(0, 999)])
        .unwrap();

    let design = svxprobe_ingest::from_path(&model).unwrap();
    let wave = svxprobe_wave::LoadedWave::open(&trace).unwrap();
    let cp = CrossProbe::build_cached(design, &wave, &opts, Path::new(&model), Path::new(&trace));

    assert_eq!(
        cp.design().wave_index.len(),
        1,
        "served from the fabricated cache, not re-matched"
    );
    assert_eq!(cp.design().wave_index.node_of(WaveSignalRef(999)), Some(0));
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

/// A source click on a modport member token (`valid` inside `modport mem (...)`,
/// the modport pin's def_range) must still reach the member's waveform: the pin
/// is a view node, and the wave link lives on its same-path sibling (the member
/// Var), which the selection's path-equivalence set carries (#64).
fn modport_pin_click_reaches_wave(ext: &str) {
    let cp = build(ext);
    // The pin shares the member's path; it is the node whose parent is the
    // modport-qualified Interface node (the member Var's parent is not).
    let pin = cp
        .design()
        .nodes_at_path("picorv32_soc.g_lane[0].bus.valid")
        .iter()
        .filter_map(|&id| cp.design().node(id))
        .find(|n| {
            n.parent
                .and_then(|p| cp.design().node(p))
                .is_some_and(|p| p.modport.is_some())
        })
        .expect("the modport pin node");
    let def = pin.def_range.expect("pin has a def range");

    let res = cp
        .from_source(def.file, def.start.offset as usize)
        .expect("modport member token resolves");
    assert_eq!(
        path_of(&cp, res.selection.anchor),
        "picorv32_soc.g_lane[0].bus.valid",
        "[{ext}] anchors the member path"
    );
    assert!(
        matches!(cp.to_wave(&res.selection), WaveTarget::Linked { .. }),
        "[{ext}] a modport pin click reaches the member's wave link"
    );
}

/// A source click on a *usage* inside a process resolves to the signal it names,
/// not the enclosing process block a declaration-span lookup would return (#225).
/// Because the picorv32 core is instantiated on two lanes, the sibling lane is also
/// offered as a picker alternative — the same disambiguation as the `picker` test,
/// but reached from a usage the model previously had no span for.
fn usage_resolves_to_signal(ext: &str) {
    let cp = build(ext);
    let d = cp.design();
    let pv = d
        .doc
        .files
        .iter()
        .find(|f| f.path.ends_with("picorv32.v"))
        .expect("picorv32.v in files")
        .id;
    let is_signal = |id: NodeId| {
        matches!(
            d.node(id).map(|n| n.kind),
            Some(NodeKind::Var | NodeKind::Net | NodeKind::Port)
        )
    };
    // A local-signal usage whose enclosing source node is a process, so a pre-#225
    // declaration-span lookup would have landed on that block instead.
    let target = d
        .name_refs_in_file(pv)
        .into_iter()
        .find(|r| {
            r.class == NameClass::Signal
                && !r.rel.contains('.')
                && r.absolute_path().is_none()
                && d.nodes_at_path(&format!("picorv32_soc.g_lane[0].core.{}", r.rel))
                    .iter()
                    .any(|&id| is_signal(id))
                && d.nodes_in_source_range(pv, r.offset as usize, r.offset as usize + 1)
                    .iter()
                    .any(|&id| {
                        matches!(
                            d.node(id).map(|n| n.kind),
                            Some(NodeKind::Ff | NodeKind::Comb | NodeKind::Assign)
                        )
                    })
        })
        .expect("a local-signal usage inside a process");
    let (off, rel) = (target.offset as usize, target.rel.clone());

    let res = cp.from_source(pv, off).expect("usage resolves");
    assert!(
        is_signal(res.selection.anchor),
        "[{ext}] usage resolved to {:?}, expected a signal",
        d.node(res.selection.anchor).map(|n| n.kind),
    );
    let apath = path_of(&cp, res.selection.anchor);
    assert!(
        apath.ends_with(&format!(".core.{rel}")),
        "[{ext}] anchor {apath} does not name core.{rel}",
    );
    // Both core lanes are represented across anchor + alternatives.
    let mut all: Vec<String> = res.alternatives.iter().map(|&n| path_of(&cp, n)).collect();
    all.push(apath);
    assert!(
        all.iter().any(|p| p.contains("g_lane[0].core"))
            && all.iter().any(|p| p.contains("g_lane[1].core")),
        "[{ext}] both core lanes present: {all:?}",
    );
}

#[test]
fn usage_resolves_to_signal_fst() {
    usage_resolves_to_signal("fst");
}
#[test]
fn usage_resolves_to_signal_vcd() {
    usage_resolves_to_signal("vcd");
}

#[test]
fn modport_pin_click_reaches_wave_fst() {
    modport_pin_click_reaches_wave("fst");
}
#[test]
fn modport_pin_click_reaches_wave_vcd() {
    modport_pin_click_reaches_wave("vcd");
}
