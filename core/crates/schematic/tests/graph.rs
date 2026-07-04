//! Schematic extraction on the committed fixture.

use std::path::PathBuf;

use svxprobe_model::{Design, Dir, NodeId};
use svxprobe_schematic::{cone, scope_graph, PinRole, Side};

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
    // Each pin carries its canonical model path so a right-click can cross-probe
    // it to source via probe_node.
    assert_eq!(
        clk.path, "picorv32_soc.g_lane[0].core.clk",
        "pin carries its model path"
    );
    assert!(
        d.nodes_at_path(&clk.path).contains(&clk.id),
        "pin path resolves back to the port node"
    );

    // Pin side follows the declared direction: input clk west, output east.
    assert_eq!(clk.side, Side::West, "input clk on the west");
    let mv = core.ports.iter().find(|p| p.name == "mem_valid").unwrap();
    assert_eq!(mv.side, Side::East, "output mem_valid on the east");
    // Unconnected ports are hidden to keep blocks compact.
    assert!(
        core.ports.iter().all(|p| p.name != "eoi"),
        "dangling output eoi should be hidden"
    );

    // There is internal wiring, including the memory↔bus connection. Wires
    // anchor on the bus box or one of its pins (member pins since #96).
    assert!(!g.edges.is_empty(), "no wires");
    let bus = id(&d, "picorv32_soc.g_lane[0].bus");
    let bus_ids: std::collections::HashSet<NodeId> = g
        .nodes
        .iter()
        .find(|n| n.id == bus)
        .map(|n| n.ports.iter().map(|p| p.id).chain([bus]).collect())
        .unwrap_or_default();
    assert!(
        g.edges
            .iter()
            .any(|e| bus_ids.contains(&e.source) || bus_ids.contains(&e.target)),
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
fn modport_interface_port_is_a_bundle_pin_on_its_instance() {
    // #106: in the parent scope, a modport-qualified interface port shows as a
    // single bundle pin on the consuming instance box — the modport connection
    // sits in the port row alongside the module's other pins. Members stay on
    // the drilled view's bundle box.
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0]").expect("scope graph");

    let memory = g.nodes.iter().find(|n| n.label == "memory").unwrap();
    let pin = memory
        .ports
        .iter()
        .find(|p| p.path == "picorv32_soc.g_lane[0].memory.bus")
        .expect("bundle pin for the modport-qualified interface port");
    // Labelled with the bundle name plus its interface type and modport view.
    assert_eq!(pin.name, "bus (mem_if.mem)");
    // Side from the members' direction majority (mem: 6 in / 2 out → west).
    assert_eq!(pin.side, Side::West);
    assert_eq!(pin.width, None, "a bundle pin carries no bit-range");
    assert!(pin.bundle, "marked as a bundle pin (drawn square)");
    // The pin is the interface-port node itself, so a right-click cross-probes
    // it directly via probe_node.
    assert!(
        d.nodes_at_path(&pin.path).contains(&pin.id),
        "pin path resolves back to the interface-port node"
    );

    // The memory↔bus wire anchors bundle-pin to the instance's `mem` access
    // port (the Modport node, #96 revised) — and only once (member-level
    // edges collapse onto the same pin).
    let bus = id(&d, "picorv32_soc.g_lane[0].bus");
    let mem_view = id(&d, "picorv32_soc.g_lane[0].bus.mem");
    let pin_wires: Vec<_> = g
        .edges
        .iter()
        .filter(|e| {
            (e.source == pin.id && e.target == mem_view)
                || (e.source == mem_view && e.target == pin.id)
        })
        .collect();
    assert_eq!(
        pin_wires.len(),
        1,
        "one wire from bundle pin to the mem port"
    );
    // No leftover box-anchored wire between memory and bus.
    assert!(
        !g.edges
            .iter()
            .any(|e| (e.source == memory.id && e.target == bus)
                || (e.source == bus && e.target == memory.id)),
        "no box-corner wire remains between memory and bus"
    );

    // The bare interface instance keeps its bundle box.
    assert!(g.nodes.iter().any(|n| n.label == "bus"), "bus box remains");
}

#[test]
fn wires_carry_the_net_canonical_path() {
    // A clicked wire cross-probes its net to source/waveform by a pure model
    // lookup, so each structural edge must carry the connecting net's *canonical*
    // path (no scope-relative trimming, no bit-select) — resolvable straight back
    // through `nodes_at_path`. (#14: clickable wires that jump to source.)
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0]").expect("scope graph");

    let bus_wire = g
        .edges
        .iter()
        .find(|e| e.net.as_deref().is_some_and(|s| s.starts_with("bus.")))
        .expect("a bus wire");
    let path = bus_wire
        .net_path
        .as_deref()
        .expect("bus wire carries a net_path");
    // It is the absolute model path (scope-qualified), not the relative label.
    assert!(
        path.starts_with("picorv32_soc.g_lane[0].bus."),
        "net_path should be the canonical path, got {path:?}"
    );
    // And it resolves straight back to a real model node (the lookup #14 relies on).
    assert!(
        !d.nodes_at_path(path).is_empty(),
        "net_path {path:?} did not resolve to any node"
    );

    // Bit-selected wires keep the bare signal path (the select is wire-level, not a
    // node): `core_trap[0]`'s label carries the bit, its net_path does not.
    let top = scope_graph(&d, "picorv32_soc").expect("top scope graph");
    let ct = top
        .edges
        .iter()
        .find(|e| {
            e.net
                .as_deref()
                .is_some_and(|s| s.starts_with("core_trap["))
        })
        .expect("a core_trap bit-select wire");
    assert_eq!(
        ct.net_path.as_deref(),
        Some("picorv32_soc.core_trap"),
        "bit-select wire's net_path should be the bare signal path"
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
fn interface_instance_is_a_bundle_box() {
    // An interface instance renders as its own box, carrying the Interface kind
    // (so the frontend draws a bundle shape) and its interface type as the module
    // sublabel (mem_if). It is a leaf bundle, not a navigable scope.
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0]").expect("scope graph");

    let bus = g
        .nodes
        .iter()
        .find(|n| n.label == "bus")
        .expect("a bus box in the lane scope");
    assert_eq!(
        bus.kind,
        svxprobe_model::NodeKind::Interface,
        "bus is an interface"
    );
    assert_eq!(
        bus.module.as_deref(),
        Some("mem_if"),
        "interface type sublabel"
    );
    assert!(!bus.expandable, "an interface bundle is not drillable");
}

#[test]
fn bare_interface_exposes_one_access_port_per_connection_style() {
    // #96 (revised): the interface instance bundle carries one aggregate port
    // per *access path*, both structural facts:
    // - a port named after the modport view (`mem`) for a modport-qualified
    //   consumer, wired once to that consumer's bundle pin;
    // - a port named after the interface (`mem_if`) for raw member taps,
    //   fanning out to each tapping pin (picorv32's scalar ports).
    // No per-member pins on the bundle.
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0]").expect("scope graph");
    let bus = g.nodes.iter().find(|n| n.label == "bus").expect("bus box");
    let pin = |name: &str| {
        bus.ports
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "no {name} pin: {:?}",
                    bus.ports.iter().map(|p| &p.name).collect::<Vec<_>>()
                )
            })
    };

    // Exactly three ports: the interface's own clk, the raw-access aggregate,
    // and the used modport view. No member pins, no port for the unused view.
    let names: Vec<&str> = bus.ports.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names.len(), 3, "ports: {names:?}");
    for member in ["valid", "addr", "ready"] {
        assert!(
            !names.contains(&member),
            "no member pin {member}: {names:?}"
        );
    }
    assert!(
        !names.contains(&"core"),
        "unused view gets no port: {names:?}"
    );

    // The view port is the Modport node itself (cross-probes to the modport).
    let mem = pin("mem");
    assert!(
        d.nodes_at_path("picorv32_soc.g_lane[0].bus.mem")
            .contains(&mem.id),
        "mem port is the modport node"
    );
    assert_eq!(mem.side, Side::East, "mostly-in view faces its consumer");

    // Both aggregate ports are marked as bundles (drawn square, not as the
    // directional triangle of a normal scalar/bus pin); the interface's own
    // clk port stays a normal pin.
    assert!(mem.bundle, "view port is a bundle pin");
    assert!(pin("mem_if").bundle, "raw access port is a bundle pin");
    assert!(!pin("clk").bundle, "clk stays a normal pin");

    // One wire between the mem port and the consumer's bundle pin (#106) —
    // the whole-interface and member-level edges all collapse onto it.
    let memory = g.nodes.iter().find(|n| n.label == "memory").unwrap();
    let bundle = memory
        .ports
        .iter()
        .find(|p| p.path == "picorv32_soc.g_lane[0].memory.bus")
        .expect("memory bundle pin");
    let mem_wires = g
        .edges
        .iter()
        .filter(|e| {
            (e.source == mem.id && e.target == bundle.id)
                || (e.source == bundle.id && e.target == mem.id)
        })
        .count();
    assert_eq!(mem_wires, 1, "one wire mem port <-> memory bundle pin");

    // The raw-access port carries the interface's name and fans out to the
    // core's individual pins — one wire per tapped member.
    let raw = pin("mem_if");
    assert_eq!(
        raw.side,
        Side::West,
        "mostly-driven-into raw port faces west"
    );
    assert_eq!(
        raw.path, "picorv32_soc.g_lane[0].bus",
        "raw port cross-probes to the interface instance"
    );
    let core = g.nodes.iter().find(|n| n.label == "core").unwrap();
    let core_pins: std::collections::HashSet<NodeId> = core.ports.iter().map(|p| p.id).collect();
    let fanout = g
        .edges
        .iter()
        .filter(|e| {
            (e.source == raw.id && core_pins.contains(&e.target))
                || (e.target == raw.id && core_pins.contains(&e.source))
        })
        .count();
    assert!(fanout >= 5, "raw port fans out to core pins, got {fanout}");
}

#[test]
fn modport_interface_port_has_directional_pins() {
    // Inside the consumer, its modport-qualified interface port renders as a
    // bundle box with one directional pin per modport member (#64). Sides are
    // mirrored like boundary pins: an `in` member enters the consumer, so its
    // pin faces the design (east); an `out` member is entered from the west.
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0].memory").expect("scope graph");

    let bus = g
        .nodes
        .iter()
        .find(|n| n.label == "bus")
        .expect("the consumer's interface-port bundle box");
    assert_eq!(bus.kind, svxprobe_model::NodeKind::Interface);
    assert_eq!(bus.module.as_deref(), Some("mem_if"), "interface sublabel");

    let pin = |name: &str| {
        bus.ports
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("pin {name} on the bundle: {:?}", bus.ports))
    };
    assert_eq!(pin("valid").side, Side::East, "in member faces the design");
    assert_eq!(
        pin("ready").side,
        Side::West,
        "out member entered from west"
    );
    assert_eq!(pin("addr").width.as_deref(), Some("[31:0]"));
    // A pin is a view of the underlying bundle member: its path is the member's
    // canonical path, so a click cross-probes to the real signal (and its wave).
    assert_eq!(pin("valid").path, "picorv32_soc.g_lane[0].bus.valid");

    // The consumer's logic wires to the pins: the memory FF loads `valid` from
    // the bundle and drives `ready` back into it.
    let ff = g
        .nodes
        .iter()
        .find(|n| n.kind == svxprobe_model::NodeKind::Ff)
        .expect("the memory FF");
    let touches = |pin_id| {
        g.edges
            .iter()
            .any(|e| e.source == pin_id || e.target == pin_id)
    };
    assert!(touches(pin("valid").id), "valid pin is wired");
    assert!(touches(pin("ready").id), "ready pin is wired");
    let valid_wire = g
        .edges
        .iter()
        .find(|e| e.source == pin("valid").id || e.target == pin("valid").id)
        .unwrap();
    let ff_pins: Vec<_> = ff.ports.iter().map(|p| p.id).collect();
    assert!(
        ff_pins.contains(&valid_wire.source) || ff_pins.contains(&valid_wire.target),
        "valid wires to the FF"
    );
    assert_eq!(
        valid_wire.net_path.as_deref(),
        Some("picorv32_soc.g_lane[0].bus.valid"),
        "wire cross-probes to the bundle member"
    );

    // The bare interface *instance* keeps no member pins (#96 revised): its
    // connections aggregate into per-access ports (`mem` for the modport
    // consumer, `mem_if` for raw taps) — unlike the consumer-side bundle
    // above, whose pins are the directional member views of one modport.
    let lane = scope_graph(&d, "picorv32_soc.g_lane[0]").expect("lane graph");
    let inst = lane.nodes.iter().find(|n| n.label == "bus").unwrap();
    assert!(
        inst.ports.iter().all(|p| p.name != "valid"),
        "instance aggregates access ports, no member pins: {:?}",
        inst.ports
    );
}

#[test]
fn modport_interface_port_carries_its_view_on_the_node() {
    // #106: the drilled view's bundle box carries the modport view so the
    // frontend can place it at the boundary frame (a modport-qualified port is
    // the module's window to the outside, like its other ports) and sublabel
    // it `mem_if.mem`. A bare interface instance carries none.
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0].memory").expect("scope graph");
    let bus = g.nodes.iter().find(|n| n.label == "bus").unwrap();
    assert_eq!(
        bus.modport.as_deref(),
        Some("mem"),
        "view on the bundle box"
    );

    let lane = scope_graph(&d, "picorv32_soc.g_lane[0]").expect("lane graph");
    let inst = lane.nodes.iter().find(|n| n.label == "bus").unwrap();
    assert_eq!(inst.modport, None, "bare instance carries no view");
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
fn inferred_ff_shows_clock_and_hides_dangling_output() {
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
    // `lane_state` is written by the FF but read by nothing in the scope, so its
    // dangling output pin is hidden (like unconnected module ports).
    assert!(
        ff.ports.iter().all(|p| p.side != Side::East),
        "dangling lane_state output should be hidden: {:?}",
        ff.ports.iter().map(|p| &p.name).collect::<Vec<_>>()
    );
    // The FF clock pin is wired into the design (to the boundary clk).
    assert!(
        top.edges
            .iter()
            .any(|e| e.source == clk.id || e.target == clk.id),
        "FF clock is not wired"
    );
}

#[test]
fn instance_output_wires_to_ff_read_pin() {
    // #116: `core_trap` is driven by each core's `trap` output port and read by
    // the lane FF — no logic box drives it, so the signal-join must fold plain
    // instance ports as drivers or the FF's read pin floats. The recorded
    // bit-selects must pair core[i] with lane i's FF only.
    let d = design();
    let top = scope_graph(&d, "picorv32_soc").expect("top graph");
    let trap = |lane: usize| id(&d, &format!("picorv32_soc.g_lane[{lane}].core.trap"));
    let ff_pin = |lane: usize| {
        let ff = top
            .nodes
            .iter()
            .find(|n| {
                n.kind == svxprobe_model::NodeKind::Ff
                    && n.path.starts_with(&format!("picorv32_soc.g_lane[{lane}]"))
            })
            .expect("lane FF");
        ff.ports
            .iter()
            .find(|p| p.name == "core_trap")
            .expect("FF core_trap read pin")
            .id
    };
    let edge_between = |a: NodeId, b: NodeId| {
        top.edges
            .iter()
            .find(|e| (e.source == a && e.target == b) || (e.source == b && e.target == a))
    };
    for lane in 0..2 {
        assert!(
            edge_between(trap(lane), ff_pin(lane)).is_some(),
            "core {lane} trap output must wire to lane {lane}'s FF read pin"
        );
    }
    // The two lanes touch different bits of `core_trap` — the selects keep the
    // cross product from wiring core 0 into lane 1 (and vice versa).
    assert!(
        edge_between(trap(0), ff_pin(1)).is_none(),
        "bit-select mismatch must not wire"
    );
    assert!(
        edge_between(trap(1), ff_pin(0)).is_none(),
        "bit-select mismatch must not wire"
    );
    // The label carries the driven bit.
    let e = edge_between(trap(0), ff_pin(0)).unwrap();
    assert_eq!(e.net.as_deref(), Some("core_trap[0]"));
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
fn leaf_instance_is_drillable() {
    // A leaf RTL module (picorv32 — no child instances or inferred FFs in its own
    // scope) must still be drillable: a module instance always has an interior (its
    // module body / I/O frame). Regression guard for the silent double-click no-op
    // (#30) where leaf instances reported expandable == false.
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0]").unwrap();

    let core = g.nodes.iter().find(|n| n.label == "core").unwrap();
    assert_eq!(
        core.kind,
        svxprobe_model::NodeKind::Instance,
        "core is a module instance"
    );
    assert!(core.expandable, "leaf instance must be drillable");

    // Non-instance boxes/pins stay non-expandable (inferred FFs, boundary ports).
    for n in g
        .nodes
        .iter()
        .filter(|n| n.kind != svxprobe_model::NodeKind::Instance)
    {
        assert!(
            !n.expandable,
            "non-instance node should not be expandable: {} ({:?})",
            n.label, n.kind
        );
    }
}

#[test]
fn leaf_core_shows_wired_internal_logic() {
    // Drilling into a leaf module (picorv32 core — no child instances) renders its
    // internal logic at process granularity: Comb + Ff boxes wired through the
    // scope-level signals they share, plus the core's own boundary pins (#33).
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0].core").expect("core scope graph");

    let combs = g
        .nodes
        .iter()
        .filter(|n| n.kind == svxprobe_model::NodeKind::Comb)
        .count();
    let assigns = g
        .nodes
        .iter()
        .filter(|n| n.kind == svxprobe_model::NodeKind::Assign)
        .count();
    let ffs: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == svxprobe_model::NodeKind::Ff)
        .collect();
    assert!(combs > 0, "drilled core has no comb (always @*) boxes");
    assert!(assigns > 0, "drilled core has no assign nodes");
    assert!(
        ffs.len() >= 10,
        "drilled core should expose its clocked-always FFs, got {}",
        ffs.len()
    );

    // The core's clock input appears as a boundary pin.
    assert!(
        g.nodes
            .iter()
            .any(|n| n.kind == svxprobe_model::NodeKind::Port && n.label == "clk"),
        "no clk boundary pin in the drilled core"
    );

    // The internal logic is actually wired (signal-join), not floating boxes.
    assert!(!g.edges.is_empty(), "no internal wires in the drilled core");

    // Connected FF outputs (read by other logic in the core) are still shown —
    // only *dangling* outputs are pruned.
    assert!(
        ffs.iter()
            .any(|ff| ff.ports.iter().any(|p| p.side == Side::East)),
        "no FF shows a (connected) output pin in the drilled core"
    );

    // Every FF gets its own clk pin (guards the per-(box,signal) allocator, #32).
    let clk_pins: Vec<NodeId> = ffs
        .iter()
        .filter_map(|ff| ff.ports.iter().find(|p| p.name == "clk").map(|p| p.id))
        .collect();
    let uniq: std::collections::HashSet<NodeId> = clk_pins.iter().copied().collect();
    assert_eq!(uniq.len(), clk_pins.len(), "two FFs share a clk pin");

    // No wire joins two pins of the *same* box (no self-loop).
    let mut pin_box = std::collections::HashMap::new();
    for n in &g.nodes {
        for p in &n.ports {
            pin_box.insert(p.id, n.id);
        }
    }
    for e in &g.edges {
        if let (Some(&sb), Some(&tb)) = (pin_box.get(&e.source), pin_box.get(&e.target)) {
            assert_ne!(sb, tb, "edge {} joins two pins of one box", e.id);
        }
    }
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

#[test]
fn ff_clock_pin_carries_the_clk_role() {
    // #59: the FF's clock pin is tagged from the model fact (`Node.type_` holds
    // the timing-control signal name emitted by the harness), not a name guess.
    let d = design();
    let top = scope_graph(&d, "picorv32_soc").expect("top graph");
    let ff = top
        .nodes
        .iter()
        .find(|n| n.kind == svxprobe_model::NodeKind::Ff)
        .expect("lane_state FF");
    let clk = ff
        .ports
        .iter()
        .find(|p| p.name == "clk")
        .expect("FF clock pin");
    assert_eq!(clk.role, Some(PinRole::Clk));
    // The fixture FFs are sync-reset (no async-reset model fact), so no other
    // pin may carry a role — in particular `resetn` must NOT be tagged by name.
    assert!(
        ff.ports.iter().all(|p| p.name == "clk" || p.role.is_none()),
        "only the clock is role-tagged: {:?}",
        ff.ports
            .iter()
            .map(|p| (&p.name, p.role))
            .collect::<Vec<_>>()
    );
}

#[test]
fn ff_reset_and_latch_enable_pins_carry_roles() {
    // #59: `Node.reset` (async-reset path on an FF) and `Node.enable` (gating
    // condition path on a latch) are model facts from the harness; the matching
    // synthesized pins carry the corresponding role.
    let doc = r#"{
        "schema_version": 1,
        "design": "t",
        "files": [{"id": 0, "path": "t.sv"}],
        "nodes": [
            {"id":0,"kind":"Instance","name":"t","path":"t","parent":null,
             "children":[1,2,3,4,5,6,7],"symbol_key":"t"},
            {"id":1,"kind":"Var","name":"clk","path":"t.clk","parent":0,
             "children":[],"symbol_key":"t.clk","type":"logic"},
            {"id":2,"kind":"Var","name":"rst_n","path":"t.rst_n","parent":0,
             "children":[],"symbol_key":"t.rst_n","type":"logic"},
            {"id":3,"kind":"Var","name":"d","path":"t.d","parent":0,
             "children":[],"symbol_key":"t.d","type":"logic"},
            {"id":4,"kind":"Var","name":"en","path":"t.en","parent":0,
             "children":[],"symbol_key":"t.en","type":"logic"},
            {"id":5,"kind":"Var","name":"q","path":"t.q","parent":0,
             "children":[],"symbol_key":"t.q","type":"logic"},
            {"id":6,"kind":"FF","name":"FF","path":"t.$ff6","parent":0,
             "children":[],"symbol_key":"t.$ff6","type":"clk","reset":"t.rst_n"},
            {"id":7,"kind":"Latch","name":"Latch","path":"t.$latch7","parent":0,
             "children":[],"symbol_key":"t.$latch7","enable":"t.en"}
        ],
        "edges": [
            {"id":0,"port":6,"endpoint":1,"dir":"in"},
            {"id":1,"port":6,"endpoint":2,"dir":"in"},
            {"id":2,"port":6,"endpoint":3,"dir":"in"},
            {"id":3,"port":6,"endpoint":5,"dir":"out"},
            {"id":4,"port":7,"endpoint":4,"dir":"in"},
            {"id":5,"port":7,"endpoint":3,"dir":"in"},
            {"id":6,"port":7,"endpoint":5,"dir":"out"}
        ]
    }"#;
    let d = svxprobe_ingest::from_slice(doc.as_bytes()).unwrap();
    let g = scope_graph(&d, "t").expect("scope graph");

    let role_of = |kind: svxprobe_model::NodeKind, pin: &str| {
        g.nodes
            .iter()
            .find(|n| n.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} box"))
            .ports
            .iter()
            .find(|p| p.name == pin)
            .unwrap_or_else(|| panic!("no {pin} pin on {kind:?}"))
            .role
    };
    use svxprobe_model::NodeKind::{Ff, Latch};
    assert_eq!(role_of(Ff, "clk"), Some(PinRole::Clk));
    assert_eq!(
        role_of(Ff, "rst_n"),
        Some(PinRole::Reset),
        "path matches Node.reset"
    );
    assert_eq!(role_of(Ff, "d"), None, "plain data input is untagged");
    assert_eq!(
        role_of(Latch, "en"),
        Some(PinRole::Enable),
        "path matches Node.enable"
    );
    assert_eq!(role_of(Latch, "d"), None, "latch data input is untagged");
}
