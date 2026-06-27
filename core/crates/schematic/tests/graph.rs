//! Schematic extraction on the committed fixture.

use std::path::PathBuf;

use svxprobe_model::{Design, Dir, NodeId};
use svxprobe_schematic::{cone, scope_graph, Side};

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

    // Module type is recovered from the defining file's basename (picorv32.v).
    assert_eq!(core.module.as_deref(), Some("picorv32"), "core module type");

    // A bus pin carries its declared bit-range; a scalar pin has none.
    let mem_addr = core.ports.iter().find(|p| p.name == "mem_addr").unwrap();
    assert_eq!(mem_addr.width.as_deref(), Some("[31:0]"), "mem_addr width");
    let clk = core.ports.iter().find(|p| p.name == "clk").unwrap();
    assert_eq!(clk.width, None, "scalar pin has no width");

    // Pin side follows the declared direction: input clk west, output east.
    assert_eq!(clk.side, Side::West, "input clk on the west");
    let mv = core.ports.iter().find(|p| p.name == "mem_valid").unwrap();
    assert_eq!(mv.side, Side::East, "output mem_valid on the east");
    // Unconnected ports are hidden to keep blocks compact.
    assert!(
        core.ports.iter().all(|p| p.name != "eoi"),
        "dangling output eoi should be hidden"
    );

    // There is internal wiring, including the memory↔bus connection.
    assert!(!g.edges.is_empty(), "no wires");
    let bus = id(&d, "picorv32_soc.g_lane[0].bus");
    assert!(
        g.edges.iter().any(|e| e.source == bus || e.target == bus),
        "no edge touches the bus instance"
    );

    // Wires carry the connecting net name, relative to the scope (e.g. bus.valid).
    assert!(
        g.edges
            .iter()
            .any(|e| e.net.as_deref().is_some_and(|s| s.starts_with("bus."))),
        "no wire labeled with a bus net: {:?}",
        g.edges
            .iter()
            .filter_map(|e| e.net.as_deref())
            .collect::<Vec<_>>()
    );
}

#[test]
fn bus_wires_carry_bit_select_labels() {
    // `core_trap` is `logic[1:0]`; at the top scope each lane's FF and core tap a
    // distinct bit, so the wires must be labelled `core_trap[0]` / `core_trap[1]`
    // rather than a bare, indistinguishable `core_trap`.
    let d = design();
    let g = scope_graph(&d, "picorv32_soc").expect("top scope graph");

    let ct_labels: std::collections::HashSet<&str> = g
        .edges
        .iter()
        .filter_map(|e| e.net.as_deref())
        .filter(|n| n.starts_with("core_trap"))
        .collect();

    assert!(
        ct_labels.contains("core_trap[0]"),
        "missing core_trap[0]: {ct_labels:?}"
    );
    assert!(
        ct_labels.contains("core_trap[1]"),
        "missing core_trap[1]: {ct_labels:?}"
    );
}

#[test]
fn constant_tied_inputs_show_their_literal() {
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0]").unwrap();
    let core = g.nodes.iter().find(|n| n.label == "core").unwrap();
    // irq (tied to 32'd0) is still shown as a pin...
    let irq = core
        .ports
        .iter()
        .find(|p| p.name == "irq")
        .expect("const-tied irq should be shown");
    // ...driven by a constant-source node (32'd0) wired into it from outside.
    let cnode = g
        .nodes
        .iter()
        .find(|n| {
            n.constant.as_deref() == Some("32'd0")
                && g.edges
                    .iter()
                    .any(|e| e.source == n.id && e.target == irq.id)
        })
        .expect("a 32'd0 constant source wired to irq");
    assert_eq!(
        cnode.kind,
        svxprobe_model::NodeKind::Port,
        "const node kind"
    );
}

#[test]
fn generate_blocks_are_flattened_at_top() {
    let d = design();
    // The generate array is dissolved: both lanes' leaf instances appear at the
    // top, not a single g_lane group box.
    let top = scope_graph(&d, "picorv32_soc").expect("top graph");
    let labels: Vec<&str> = top.nodes.iter().map(|n| n.label.as_str()).collect();
    assert!(
        !labels.contains(&"g_lane"),
        "g_lane should be flattened: {labels:?}"
    );
    // Flattened genblock iterations carry their genblock segment (mirroring wire
    // labels) so the two lanes stay distinct instead of both reading bare `core`.
    assert!(
        labels.contains(&"g_lane[0].core") && labels.contains(&"g_lane[1].core"),
        "both lane cores shown scope-relative: {labels:?}"
    );
    assert!(
        labels.contains(&"g_lane[0].memory") && labels.contains(&"g_lane[1].memory"),
        "both lane memories shown scope-relative: {labels:?}"
    );
    // FF boxes are disambiguated the same way: each lane's FF carries its segment.
    let ff_labels: Vec<&str> = top
        .nodes
        .iter()
        .filter(|n| n.kind == svxprobe_model::NodeKind::Ff)
        .map(|n| n.label.as_str())
        .collect();
    assert!(
        ff_labels.iter().all(|l| l.starts_with("g_lane[")),
        "lane FF labels carry their genblock segment: {ff_labels:?}"
    );
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
fn top_scope_has_boundary_io_pins() {
    let d = design();
    let top = scope_graph(&d, "picorv32_soc").expect("top graph");

    // The design's own ports appear as boundary pins (NodeKind::Port boxes).
    let pin = |name: &str| {
        top.nodes
            .iter()
            .find(|n| n.kind == svxprobe_model::NodeKind::Port && n.label == name)
    };
    let clk = pin("clk").expect("clk boundary pin");
    let trap = pin("core_trap").expect("core_trap boundary pin");

    // Inputs face the design from the west frame (pin on the east); outputs the
    // reverse — so the layout puts inputs left, outputs right.
    assert_eq!(clk.ports[0].side, Side::East, "input clk pin faces east");
    assert_eq!(
        trap.ports[0].side,
        Side::West,
        "output core_trap pin faces west"
    );

    // The boundary pins are actually wired into the design.
    assert!(
        top.edges
            .iter()
            .any(|e| e.source == clk.id || e.target == clk.id),
        "clk boundary pin is not connected"
    );
}

#[test]
fn inferred_ff_is_a_box_with_clock_and_output() {
    let d = design();
    let top = scope_graph(&d, "picorv32_soc").expect("top graph");
    let ffs: Vec<_> = top
        .nodes
        .iter()
        .filter(|n| n.kind == svxprobe_model::NodeKind::Ff)
        .collect();
    assert_eq!(ffs.len(), 2, "one lane_state FF per lane");
    let ff = ffs[0];
    let clk = ff
        .ports
        .iter()
        .find(|p| p.name == "clk")
        .expect("FF clock pin");
    assert_eq!(clk.side, Side::West, "clock on the west");
    let q = ff
        .ports
        .iter()
        .find(|p| p.name == "lane_state")
        .expect("FF output pin");
    assert_eq!(q.side, Side::East, "Q on the east");
    // The FF clock pin is wired into the design (to the boundary clk).
    assert!(
        top.edges
            .iter()
            .any(|e| e.source == clk.id || e.target == clk.id),
        "FF clock is not wired"
    );
}

#[test]
fn shared_signal_gives_each_ff_a_distinct_pin() {
    let d = design();
    let top = scope_graph(&d, "picorv32_soc").expect("top graph");
    let ffs: Vec<_> = top
        .nodes
        .iter()
        .filter(|n| n.kind == svxprobe_model::NodeKind::Ff)
        .collect();
    assert_eq!(ffs.len(), 2, "one lane_state FF per lane");

    // Both lane FFs are clocked by the *same* boundary clk net — the collision
    // case. Each must still get its own synthesized clk pin id, or their wires
    // would merge into one.
    let clk_pin = |ff: &svxprobe_schematic::SchNode| {
        ff.ports
            .iter()
            .find(|p| p.name == "clk")
            .expect("FF clock pin")
            .id
    };
    assert_ne!(
        clk_pin(ffs[0]),
        clk_pin(ffs[1]),
        "two FFs sharing clk must have distinct clk pin ids"
    );

    // Within a single FF, every synthesized pin is distinct too.
    let pin_ids: Vec<NodeId> = ffs[0].ports.iter().map(|p| p.id).collect();
    let unique: std::collections::HashSet<NodeId> = pin_ids.iter().copied().collect();
    assert_eq!(unique.len(), pin_ids.len(), "FF pins must be distinct");
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
