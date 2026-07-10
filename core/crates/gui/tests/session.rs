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

#[test]
fn hierarchy_tree_is_lazy_and_navigable() {
    let s = session();

    // depth 1: the top plus its direct structural children. A generate-for's
    // array container (`g_lane`) is hoisted away (#107) — the per-iteration
    // scopes are the tree levels, mirroring the schematic's GenBlock dissolve
    // but stopping at the iterations. Grandchildren are not fetched but
    // flagged expandable so the frontend loads them lazily.
    let root = s.hierarchy_tree("picorv32_soc", 1).expect("tree root");
    assert_eq!(root.label, "picorv32_soc");
    assert_eq!(root.path, "picorv32_soc");
    assert!(root.expandable);
    let labels: Vec<&str> = root.children.iter().map(|c| c.label.as_str()).collect();
    assert!(!labels.contains(&"g_lane"), "container hoisted: {labels:?}");
    assert!(labels.contains(&"g_lane[1]"), "children: {labels:?}");
    let lane0 = root
        .children
        .iter()
        .find(|c| c.label == "g_lane[0]")
        .expect("iteration child");
    assert!(lane0.expandable, "iteration has instances below");
    assert!(lane0.children.is_empty(), "depth-1 stops here (lazy)");

    // depth 2 reaches the iterations' instances in one call.
    let deep = s.hierarchy_tree("picorv32_soc", 2).expect("tree root");
    let lane0 = deep
        .children
        .iter()
        .find(|c| c.label == "g_lane[0]")
        .unwrap();
    assert!(
        lane0.children.iter().any(|c| c.label == "core"),
        "instances: {:?}",
        lane0.children.iter().map(|c| &c.label).collect::<Vec<_>>()
    );

    // Expanding a child = re-querying with its path (what the frontend does).
    let lane = s
        .hierarchy_tree("picorv32_soc.g_lane[0]", 1)
        .expect("lane subtree");
    let labels: Vec<&str> = lane.children.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"core"), "children: {labels:?}");
    assert!(labels.contains(&"memory"), "children: {labels:?}");
    // A bare interface bundle is a navigable scope now (#97) — it drills into
    // its modport views — so the tree lists it alongside the instances.
    assert!(labels.contains(&"bus"), "children: {labels:?}");
    let core = lane.children.iter().find(|c| c.label == "core").unwrap();
    assert_eq!(core.module.as_deref(), Some("picorv32"), "module sublabel");

    // Every tree node's path opens as a schematic scope.
    for c in &lane.children {
        assert!(s.scope_graph(&c.path).is_some(), "{} is navigable", c.path);
    }

    // Unknown scopes yield None; a bare interface bundle resolves (#97).
    assert!(s.hierarchy_tree("nope", 1).is_none());
    assert!(s.hierarchy_tree("picorv32_soc.g_lane[0].bus", 1).is_some());
}

#[test]
fn elaborate_and_load_runs_the_harness() {
    // Requires `svxprobe-elaborate` on PATH — the app's runtime contract for
    // designlist loading (#93). Skip when absent (e.g. the Rust CI job carries
    // no Python env); everything past the subprocess boundary is the same
    // ingest/load path the other tests cover via Session::load.
    let available = std::process::Command::new("svxprobe-elaborate")
        .arg("--help")
        .output()
        .is_ok_and(|o| o.status.success());
    if !available {
        eprintln!("skipping: svxprobe-elaborate not on PATH");
        return;
    }
    let s = Session::elaborate_and_load(
        &fixture("picorv32_soc.f"),
        "picorv32_soc",
        &[],
        &fixture("traces/picorv32_soc.fst"),
        vec!["TOP".into(), "tb".into(), "soc_pkg".into()],
        repo_root(),
    )
    .unwrap();
    assert_eq!(s.design_top(), "picorv32_soc");
    assert!(s.scope_graph("picorv32_soc.g_lane[0]").is_some());

    // A bad top must surface the harness error, not a silent empty design.
    let err = Session::elaborate_and_load(
        &fixture("picorv32_soc.f"),
        "no_such_top",
        &[],
        &fixture("traces/picorv32_soc.fst"),
        vec![],
        repo_root(),
    );
    assert!(err.is_err(), "unknown top should fail loudly");
}
