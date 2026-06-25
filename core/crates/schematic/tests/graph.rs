//! Schematic extraction on the committed fixture.

use std::path::PathBuf;

use svxprobe_model::{Design, Dir, NodeId};
use svxprobe_schematic::{cone, scope_graph};

fn design() -> Design {
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/picorv32_soc/golden/hierarchy.json");
    svxprobe_ingest::from_path(golden).unwrap()
}

fn id(d: &Design, path: &str) -> NodeId {
    d.nodes_at_path(path)[0]
}

#[test]
fn scope_graph_has_boxes_and_wires() {
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0]").expect("scope graph");

    let labels: Vec<&str> = g.nodes.iter().map(|n| n.label.as_str()).collect();
    assert!(labels.contains(&"core"), "boxes: {labels:?}");
    assert!(labels.contains(&"memory"), "boxes: {labels:?}");
    assert!(labels.contains(&"bus"), "boxes: {labels:?}");

    // The core box exposes pins.
    let core = g.nodes.iter().find(|n| n.label == "core").unwrap();
    assert!(
        core.ports.iter().any(|p| p.name == "mem_valid"),
        "core pins"
    );

    // There is internal wiring, including the memory↔bus connection.
    assert!(!g.edges.is_empty(), "no wires");
    let bus = id(&d, "picorv32_soc.g_lane[0].bus");
    assert!(
        g.edges.iter().any(|e| e.source == bus || e.target == bus),
        "no edge touches the bus instance"
    );
}

#[test]
fn generate_blocks_are_expandable_boxes() {
    let d = design();
    // Top scope: the generate array is a single expandable group box.
    let top = scope_graph(&d, "picorv32_soc").expect("top graph");
    let g_lane = top
        .nodes
        .iter()
        .find(|n| n.label == "g_lane")
        .expect("g_lane box at top");
    assert!(g_lane.expandable, "generate array should be expandable");

    // Expanding it reveals the two lane blocks, themselves expandable.
    let lanes = svxprobe_schematic::expand(&d, g_lane.id).unwrap();
    assert_eq!(lanes.nodes.len(), 2, "two lanes");
    assert!(lanes.nodes.iter().all(|n| n.expandable), "lanes expandable");

    // Expanding a lane reveals the leaf instances.
    let lane0 = lanes.nodes.iter().find(|n| n.label == "g_lane[0]").unwrap();
    let inside = svxprobe_schematic::expand(&d, lane0.id).unwrap();
    let labels: Vec<&str> = inside.nodes.iter().map(|n| n.label.as_str()).collect();
    assert!(labels.contains(&"core") && labels.contains(&"memory") && labels.contains(&"bus"));
}

#[test]
fn expand_matches_scope_graph() {
    let d = design();
    let g0 = id(&d, "picorv32_soc.g_lane[0]");
    let via_expand = svxprobe_schematic::expand(&d, g0).unwrap();
    let via_scope = scope_graph(&d, "picorv32_soc.g_lane[0]").unwrap();
    assert_eq!(via_expand, via_scope);
}

#[test]
fn cone_reaches_the_driver() {
    let d = design();
    // bus.valid is driven by core.mem_valid; its cone should reach the core box.
    let valid = id(&d, "picorv32_soc.g_lane[0].bus.valid");
    let c = cone(&d, valid, Dir::Inout, 2);
    assert!(!c.edges.is_empty(), "cone has no edges");
    assert!(
        c.nodes.iter().any(|n| n.label == "core"),
        "cone of bus.valid did not reach core"
    );
}
