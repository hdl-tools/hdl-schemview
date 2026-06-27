//! Round-trip the committed tier-1 golden hierarchy through ingest.

use std::path::PathBuf;

use svxprobe_model::NodeKind;

fn golden() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/picorv32_soc/golden/hierarchy.json")
}

#[test]
fn ingests_committed_golden() {
    let design = svxprobe_ingest::from_path(golden()).expect("ingest golden");
    assert_eq!(design.doc.design, "picorv32_soc");

    // The generate wrapper expands to two lanes; both must be present.
    assert!(!design.nodes_at_path("picorv32_soc.g_lane[0]").is_empty());
    assert!(!design.nodes_at_path("picorv32_soc.g_lane[1]").is_empty());
    assert!(!design
        .nodes_at_path("picorv32_soc.g_lane[0].core")
        .is_empty());

    // The package-typed signal and the interface instance.
    assert!(!design
        .nodes_at_path("picorv32_soc.g_lane[0].lane_state")
        .is_empty());
    assert!(!design
        .nodes_at_path("picorv32_soc.g_lane[0].bus")
        .is_empty());

    // Source reverse index is populated.
    let top = &design.nodes()[0];
    let r = top.def_range.expect("top has a def_range");
    assert!(!design
        .nodes_at_source(r.file, r.start.offset as usize)
        .is_empty());
}

#[test]
fn golden_edges_carry_bit_select() {
    // `core_trap` is `logic[1:0]`; each lane taps a distinct bit (`core_trap[gi]`),
    // so the edges landing on the core_trap Var must carry per-bit selects.
    let design = svxprobe_ingest::from_path(golden()).expect("ingest golden");
    let ct = design
        .nodes_at_path("picorv32_soc.core_trap")
        .iter()
        .copied()
        .find(|&id| design.node(id).is_some_and(|n| n.kind == NodeKind::Var))
        .expect("core_trap var node");

    let selects: std::collections::HashSet<Option<&str>> = design
        .edges()
        .iter()
        .filter(|e| e.endpoint == ct)
        .map(|e| e.select.as_deref())
        .collect();

    assert!(selects.contains(&Some("[0]")), "missing [0]: {selects:?}");
    assert!(selects.contains(&Some("[1]")), "missing [1]: {selects:?}");
    // A non-indexed connection deserializes to None.
    assert!(
        design.edges().iter().any(|e| e.select.is_none()),
        "expected some scalar edges with no select"
    );
}
