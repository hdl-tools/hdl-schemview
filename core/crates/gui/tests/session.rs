//! The GUI session logic, exercised end-to-end on the fixture (no UI toolkit).

use std::path::PathBuf;

use svxprobe_gui::Session;

fn fixture(rel: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/picorv32_soc")
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn session() -> Session {
    Session::load(
        &fixture("golden/hierarchy.json"),
        &fixture("traces/picorv32_soc.fst"),
        vec!["TOP".into(), "tb".into(), "soc_pkg".into()],
        repo_root(),
    )
    .unwrap()
}

#[test]
fn scope_and_expand() {
    let s = session();
    assert_eq!(s.design_top(), "picorv32_soc");
    let g = s.scope_graph("picorv32_soc.g_lane[0]").unwrap();
    assert!(g.nodes.iter().any(|n| n.label == "core"));
    assert!(!g.edges.is_empty());
}

#[test]
fn probe_signal_links_all_views() {
    let mut s = session();
    let r = s
        .probe_signal("TOP.tb.dut.g_lane[0].bus.valid", None)
        .expect("resolves");
    assert_eq!(r.anchor.path, "picorv32_soc.g_lane[0].bus.valid");
    // Source view target present.
    let src = r.source.expect("has source");
    assert!(src.path.ends_with("mem_if.sv"));
    // Waveform target present and loadable.
    assert!(r.wave.in_trace);
    let changes = s.signal_values(r.wave.signal_ref);
    assert!(changes.len() > 2, "bus.valid should toggle");
}

#[test]
fn probe_source_picker_with_context() {
    let mut s = session();
    // lane_state decl is shared across lanes (offset 1028 in file 0).
    let no_ctx = s.probe_source(0, 1028, None).expect("resolves");
    assert_eq!(no_ctx.alternatives.len(), 1, "picker offers the other lane");

    let ctx = s
        .probe_source(0, 1028, Some("picorv32_soc.g_lane[1]"))
        .expect("resolves");
    assert_eq!(ctx.anchor.path, "picorv32_soc.g_lane[1].lane_state");
}

#[test]
fn not_in_trace_is_explicit() {
    let mut s = session();
    let r = s.probe_node("picorv32_soc.g_lane[0].core", None).unwrap();
    assert!(!r.wave.in_trace, "an instance has no waveform");
}

#[test]
fn source_text_loads() {
    let s = session();
    let g = s.scope_graph("picorv32_soc.g_lane[0]").unwrap();
    // file 0 is the wrapper; ensure we can read some source.
    let text = s.source_text(0).unwrap();
    assert!(text.contains("module"));
    let _ = g;
}
