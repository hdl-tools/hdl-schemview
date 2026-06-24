//! Round-trip the committed tier-1 golden hierarchy through ingest.

use std::path::PathBuf;

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
