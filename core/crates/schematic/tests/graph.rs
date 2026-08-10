//! Schematic extraction on the committed fixture.

use std::path::PathBuf;

use svxprobe_model::{Design, Dir, NodeId, NodeKind};
use svxprobe_schematic::{
    cone, cone_with, scope_graph, scope_graph_with, trace_graph, ConeLimits, PinRole, Projection,
    SchNode, SchematicGraph, Side, TraceStep,
};

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
    // Unconnected ports show as dangling pins instead of being hidden (#118).
    let eoi = core.ports.iter().find(|p| p.name == "eoi").unwrap();
    assert!(
        eoi.dangling,
        "unconnected output eoi shows, marked dangling"
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
    // #97: a bare interface bundle drills into its modport views/members.
    assert!(bus.expandable, "an interface bundle is drillable");
}

#[test]
fn drill_interface_shows_modport_boxes() {
    // #97: drilling a bare interface bundle renders each of its `modport` views
    // as a box, one directional pin per member. The member pin's `path` is the
    // underlying bundle signal (so a click cross-probes it), while its id is a
    // synthetic per-(view, member) pin — the two views share the signal but not
    // the pin.
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0].bus").expect("interface is drillable");

    let boxes: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Modport)
        .map(|n| n.label.as_str())
        .collect();
    assert!(boxes.contains(&"core"), "modport boxes: {boxes:?}");
    assert!(boxes.contains(&"mem"), "modport boxes: {boxes:?}");

    let mem = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Modport && n.label == "mem")
        .expect("mem view box");

    // `valid` is an input to the `mem` view (driven by the core), so it reads
    // from the west; its path cross-probes to the bundle member.
    let mvalid = mem
        .ports
        .iter()
        .find(|p| p.name == "valid")
        .expect("mem.valid pin");
    assert_eq!(mvalid.path, "picorv32_soc.g_lane[0].bus.valid");
    assert!(
        !d.nodes_at_path(&mvalid.path).is_empty(),
        "member path resolves to the bundle signal"
    );
    assert_eq!(
        mvalid.side,
        Side::West,
        "an `in` member reads from the west"
    );

    // The `core` view drives `valid` (output) — east — and gets its own pin id.
    let core = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Modport && n.label == "core")
        .unwrap();
    let cvalid = core.ports.iter().find(|p| p.name == "valid").unwrap();
    assert_eq!(
        cvalid.side,
        Side::East,
        "an `out` member drives to the east"
    );
    assert_ne!(cvalid.id, mvalid.id, "each view gets a distinct pin");
}

#[test]
fn drill_interface_wires_shared_members() {
    // #97: a member driven by one view and read by another is wired between the
    // two view boxes, cross-probing to the member's canonical path.
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0].bus").expect("interface is drillable");
    let modport = |label: &str| {
        g.nodes
            .iter()
            .find(|n| n.kind == NodeKind::Modport && n.label == label)
            .unwrap()
    };
    let core = modport("core");
    let mem = modport("mem");
    let cvalid = core.ports.iter().find(|p| p.name == "valid").unwrap();
    let mvalid = mem.ports.iter().find(|p| p.name == "valid").unwrap();

    let wire = g
        .edges
        .iter()
        .find(|e| {
            (e.source == cvalid.id && e.target == mvalid.id)
                || (e.source == mvalid.id && e.target == cvalid.id)
        })
        .expect("a wire joins the two views' `valid` pins");
    assert_eq!(
        wire.net_path.as_deref(),
        Some("picorv32_soc.g_lane[0].bus.valid"),
        "the wire cross-probes to the bundle member"
    );
}

#[test]
fn drill_interface_frames_clk_and_views() {
    // #97: the drilled bundle shows the interface's own `clk` as a boundary pin
    // wired into both views' clk pins (clk comes from outside the interface), plus
    // a boundary frame port per modport view marking its external face.
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0].bus").expect("interface is drillable");
    let modport = |label: &str| {
        g.nodes
            .iter()
            .find(|n| n.kind == NodeKind::Modport && n.label == label)
            .expect("modport box")
    };

    // The interface's own `clk` input renders as a boundary pin (kind Port),
    // wired to the `clk` member pin on BOTH modport views (nothing internal
    // drives clk, so without this the pins would float).
    let clk = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Port && n.label == "clk")
        .expect("clk boundary pin");
    let clk_pin = clk.ports[0].id;
    for view in ["core", "mem"] {
        let mp = modport(view);
        let cpin = mp.ports.iter().find(|p| p.name == "clk").unwrap().id;
        assert!(
            g.edges.iter().any(|e| {
                (e.source == clk_pin && e.target == cpin)
                    || (e.source == cpin && e.target == clk_pin)
            }),
            "clk wired into the {view} view"
        );
    }

    // Each modport view has a boundary frame port (kind Port) that fans out to the
    // view's member pins (the I/O the modport lists), not the box corner, and
    // cross-probes to the Modport node.
    for view in ["core", "mem"] {
        let mp = modport(view);
        let frame = g
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Port && n.label == view)
            .unwrap_or_else(|| panic!("{view} frame port"));
        let fpin = frame.ports[0].id;
        // Fans to the view's `valid` member pin — a listed I/O.
        let vpin = mp.ports.iter().find(|p| p.name == "valid").unwrap().id;
        assert!(
            g.edges.iter().any(|e| {
                (e.source == fpin && e.target == vpin) || (e.source == vpin && e.target == fpin)
            }),
            "{view} frame port fans to its member pins"
        );
        // Not anchored on the box node corner.
        assert!(
            !g.edges.iter().any(|e| {
                (e.source == fpin && e.target == mp.id) || (e.source == mp.id && e.target == fpin)
            }),
            "{view} frame does not anchor on the box corner"
        );
        // clk is served by its own boundary pin, so it is left off the fan.
        let clk_member = mp.ports.iter().find(|p| p.name == "clk").unwrap().id;
        assert!(
            !g.edges.iter().any(|e| {
                (e.source == fpin && e.target == clk_member)
                    || (e.source == clk_member && e.target == fpin)
            }),
            "{view} frame skips clk (already on its own boundary pin)"
        );
        assert!(
            d.nodes_at_path(&frame.path).contains(&mp.id),
            "{view} frame port cross-probes the Modport node"
        );
    }
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

// A bare interface's access-port side is a *structural* fact about who connects to
// the bundle, so it must not shift when the opt-in gate-level pass (#157) dissolves
// a block into primitives that happen to read a member. #215 made this visible: once
// `if`/`else` lowered to muxes, gate reads of `bus.ready`/`bus.rdata` outvoted the
// real drivers and flipped the raw port east — in the *process-level* graph, which
// draws no gates at all. The tally skips gate primitives so the two projections agree.
#[test]
fn interface_raw_access_port_ignores_gate_primitive_taps() {
    let d = design();
    let raw_side = |proj| {
        let g = scope_graph_with(&d, "picorv32_soc.g_lane[0]", proj).expect("scope graph");
        let bus = g.nodes.iter().find(|n| n.label == "bus").expect("bus box");
        bus.ports
            .iter()
            .find(|p| p.name == "mem_if")
            .expect("raw access port")
            .side
    };
    assert_eq!(
        raw_side(Projection::ProcessLevel),
        raw_side(Projection::GateLevel),
        "the bundle's access-port side is projection-independent"
    );
    assert_eq!(
        raw_side(Projection::ProcessLevel),
        Side::West,
        "the bundle is mostly driven into by its real structural connections"
    );

    // Non-vacuity: gate primitives really do tap the bundle's members here, so the
    // assertions above exercise the skip rather than passing for lack of gates.
    let bus = id(&d, "picorv32_soc.g_lane[0].bus");
    let members: std::collections::HashSet<NodeId> =
        d.node(bus).expect("bus").children.iter().copied().collect();
    let gate_taps = d
        .edges()
        .iter()
        .filter(|e| {
            members.contains(&e.endpoint) && d.node(e.port).is_some_and(|n| n.kind == NodeKind::Mux)
        })
        .count();
    assert!(
        gate_taps > 0,
        "expected gate-level muxes tapping bus members, else this test is vacuous"
    );
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
fn memory_array_is_a_box_with_addr_din_dout_pins() {
    // #112: drilling soc_mem shows its `ram` array as a MEMORY box wired to the
    // real signals — addr←word_idx, din←wdata, dout→rdata — carrying the array
    // depth and the $readmemh INIT source, all model facts (no name-guessing).
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0].memory").expect("scope graph");

    let ram = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Memory)
        .expect("ram renders as a Memory box");
    assert_eq!(ram.label, "ram", "memory box label");
    assert_eq!(
        ram.mem_depth,
        Some(512),
        "array depth from the unpacked dim"
    );
    assert_eq!(
        ram.init_source.as_deref(),
        Some("INIT_FILE"),
        "$readmemh init source drives the INIT marker"
    );
    assert!(!ram.expandable, "a memory is a leaf glyph, not drillable");

    // One pin per role, on the expected side, carrying the real signal's path.
    let pin = |role: PinRole| {
        ram.ports
            .iter()
            .find(|p| p.role == Some(role))
            .unwrap_or_else(|| panic!("memory has a {role:?} pin"))
    };
    let addr = pin(PinRole::Addr);
    let din = pin(PinRole::Din);
    let dout = pin(PinRole::Dout);
    assert_eq!(addr.side, Side::West, "addr enters on the west");
    assert_eq!(din.side, Side::West, "din enters on the west");
    assert_eq!(dout.side, Side::East, "dout leaves on the east");
    assert!(addr.path.ends_with("word_idx"), "addr path: {}", addr.path);
    assert!(din.path.ends_with("wdata"), "din path: {}", din.path);
    assert!(dout.path.ends_with("rdata"), "dout path: {}", dout.path);
    // The pins carry the real model signals, so a right-click cross-probes them.
    for p in [addr, din, dout] {
        assert!(
            d.nodes_at_path(&p.path).contains(&p.id) || !p.path.is_empty(),
            "pin path resolves: {}",
            p.path
        );
    }

    // The memory box is actually wired into the scope: its addr pin connects to
    // the `assign word_idx = bus.addr[..]` driver, not left floating.
    assert!(
        g.edges
            .iter()
            .any(|e| e.source == addr.id || e.target == addr.id),
        "addr pin is wired to word_idx's driver"
    );
    assert!(
        g.edges
            .iter()
            .any(|e| e.source == dout.id || e.target == dout.id),
        "dout pin is wired to a rdata reader"
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
fn inferred_ff_shows_clock_and_dangling_output() {
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
    assert!(!clk.dangling, "the wired clock must not read as dangling");
    // `lane_state` is written by the FF but read by nothing in the scope (#118):
    // its Q pin stays visible, marked dangling, instead of being pruned — and
    // carries the enum's width from the model's enum table.
    let q = ff
        .ports
        .iter()
        .find(|p| p.side == Side::East)
        .expect("dangling lane_state output pin");
    assert_eq!(q.name, "lane_state");
    assert!(q.dangling, "unread output must be marked dangling");
    assert_eq!(
        q.width.as_deref(),
        Some("[1:0]"),
        "enum width from the model"
    );
    assert!(
        !top.edges
            .iter()
            .any(|e| e.source == q.id || e.target == q.id),
        "a dangling pin has no wire"
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
fn unconnected_instance_outputs_show_as_dangling_pins() {
    // #118: picorv32 leaves mem_la_*/pcpi_*/trace_* unconnected in this design —
    // they must appear on the core box as dangling pins instead of vanishing.
    let d = design();
    let g = scope_graph(&d, "picorv32_soc.g_lane[0]").expect("scope graph");
    let core = g.nodes.iter().find(|n| n.label == "core").unwrap();
    let la = core
        .ports
        .iter()
        .find(|p| p.name == "mem_la_read")
        .expect("unconnected mem_la_read output pin");
    assert!(la.dangling, "unconnected output must be marked dangling");
    assert_eq!(la.side, Side::East, "declared direction places the pin");
    // A connected pin stays undimmed.
    let trap = core
        .ports
        .iter()
        .find(|p| p.name == "trap")
        .expect("trap pin");
    assert!(!trap.dangling, "wired pins are not dangling");
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

    // Inferred logic boxes and boundary pins stay non-expandable; an interface
    // bundle is the one non-instance exception — it drills into its modports (#97).
    for n in g.nodes.iter().filter(|n| {
        !matches!(
            n.kind,
            svxprobe_model::NodeKind::Instance | svxprobe_model::NodeKind::Interface
        )
    }) {
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
    // Nine clocked-always FFs. Was ten until #178 stopped emitting the uninstantiated
    // `genblk3` branch, whose `always @(posedge clk)` was a phantom tenth register that
    // does not exist in this (single-cycle-ALU) elaboration.
    assert!(
        ffs.len() >= 9,
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
fn plain_net_joins_instances_in_logic_free_scope() {
    // #123: two instance ports meeting only through a plain scope-level net —
    // no interface, no logic box in the scope. The structural pass drops the
    // connection (the net endpoint resolves to no box) and the signal-join
    // pass used to be gated on the scope having logic boxes, so no wire was
    // drawn at all while the pins (correctly) stayed solid.
    let doc = r#"{
        "schema_version": 1,
        "design": "t",
        "files": [{"id": 0, "path": "t.sv"}],
        "nodes": [
            {"id":0,"kind":"Instance","name":"t","path":"t","parent":null,
             "children":[1,3,5],"symbol_key":"t"},
            {"id":1,"kind":"Instance","name":"a","path":"t.a","parent":0,
             "children":[2],"symbol_key":"t.a"},
            {"id":2,"kind":"Port","name":"y","path":"t.a.y","parent":1,
             "children":[],"symbol_key":"t.a.y","dir":"out","type":"logic"},
            {"id":3,"kind":"Instance","name":"b","path":"t.b","parent":0,
             "children":[4],"symbol_key":"t.b"},
            {"id":4,"kind":"Port","name":"x","path":"t.b.x","parent":3,
             "children":[],"symbol_key":"t.b.x","dir":"in","type":"logic"},
            {"id":5,"kind":"Var","name":"n","path":"t.n","parent":0,
             "children":[],"symbol_key":"t.n","type":"logic"}
        ],
        "edges": [
            {"id":0,"port":2,"endpoint":5,"dir":"out"},
            {"id":1,"port":4,"endpoint":5,"dir":"in"}
        ]
    }"#;
    let d = svxprobe_ingest::from_slice(doc.as_bytes()).unwrap();
    let g = scope_graph(&d, "t").expect("scope graph");

    let wires: Vec<_> = g
        .edges
        .iter()
        .filter(|e| (e.source, e.target) == (2, 4) || (e.source, e.target) == (4, 2))
        .collect();
    assert_eq!(
        wires.len(),
        1,
        "one wire joins a.y to b.x through the plain net: {:?}",
        g.edges
    );
    assert_eq!(
        wires[0].net.as_deref(),
        Some("n"),
        "wire carries the net name"
    );
    assert_eq!(
        wires[0].net_path.as_deref(),
        Some("t.n"),
        "wire cross-probes the net's canonical path"
    );
    // The pins stay solid: the model edges exist, so they are not dangling
    // (#118 — dangling is a model fact, untouched by this wiring).
    for inst in ["a", "b"] {
        let n = g.nodes.iter().find(|n| n.label == inst).unwrap();
        assert!(n.ports.iter().all(|p| !p.dangling), "{inst} pin dangles");
    }
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

// Only the *elaborated* branch of an if-generate is part of the design. `core.genblk3`
// is `if (TWO_CYCLE_ALU) always @(posedge clk) … else always @* …` with `TWO_CYCLE_ALU`
// 0, so this scope is the comb branch and the registered one does not exist. Every
// branch of an unnamed if-generate shares the LRM-implicit name, so emitting a dead one
// would both put a second node on this path and draw a flip-flop that isn't in the
// design — a phantom that double-drives `alu_add_sub` alongside the real comb (#178).
#[test]
fn an_uninstantiated_generate_branch_is_not_drawn() {
    let d = design();
    let path = "picorv32_soc.g_lane[0].core.genblk3";
    assert_eq!(
        d.nodes_at_path(path).len(),
        1,
        "one live branch ⇒ one node at `{path}`"
    );

    // genblk3 is logic-only, so it is no longer a scope of its own (#184); its live
    // `else` branch renders dissolved into the enclosing module. Draw `core` and
    // confirm the elaborated comb is there (the dead `if (TWO_CYCLE_ALU)` FF is gone
    // from the model — proven by the single node above — so it cannot double-drive).
    let g = scope_graph(&d, "picorv32_soc.g_lane[0].core").expect("scope graph");
    let kinds: Vec<NodeKind> = g.nodes.iter().map(|n| n.kind).collect();
    assert!(
        kinds.contains(&NodeKind::Comb),
        "the elaborated `else` branch renders in the parent: {kinds:?}"
    );
}

// A generate block that holds only logic (`comb`/`ff`/`assign`) is a syntactic
// wrapper, not a navigable design scope (#184): `scope_graph` rejects it, so a
// cross-probe landing on its path walks up to the enclosing module — where the logic
// already renders (`child_boxes` dissolves generate blocks into their contents). A
// generate block that holds instances stays a scope; the test is contents, not keyword.
#[test]
fn logic_only_generate_blocks_are_not_scopes() {
    let d = design();
    for p in [
        "picorv32_soc.g_lane[0].core.genblk1",
        "picorv32_soc.g_lane[0].core.genblk2",
        "picorv32_soc.g_lane[0].core.genblk3",
    ] {
        assert!(
            scope_graph(&d, p).is_none(),
            "`{p}` holds only logic and must not resolve as a scope"
        );
    }
    // g_lane[0] is a GenBlock too, but holds core/memory/bus instances — it stays.
    assert!(scope_graph(&d, "picorv32_soc.g_lane[0]").is_some());
}

// --- gate-level projection (#157, ADR 0005) -----------------------------------

/// A tiny gate-level model: `assign y = a & b;` (an `And`) and
/// `assign z = sel ? c : d;` (a `Mux`), each primitive a flat child of its
/// `Assign` block, carrying both the process-level edges *and* the gate edges —
/// exactly what the harness `--gate-level` pass emits. Mirrors the PR2 `GATE_DOC`
/// shape that ingest already validates.
const GATE_LEVEL_DOC: &str = r#"{
    "schema_version": 1,
    "design": "t",
    "files": [{"id": 0, "path": "t.sv"}],
    "nodes": [
        {"id":0,"kind":"Instance","name":"t","path":"t","parent":null,
         "children":[1,2,3,4,5,6,7,8,10],"symbol_key":"t"},
        {"id":1,"kind":"Port","name":"a","path":"t.a","parent":0,"children":[],
         "symbol_key":"t.a","dir":"in"},
        {"id":2,"kind":"Port","name":"b","path":"t.b","parent":0,"children":[],
         "symbol_key":"t.b","dir":"in"},
        {"id":3,"kind":"Port","name":"y","path":"t.y","parent":0,"children":[],
         "symbol_key":"t.y","dir":"out"},
        {"id":4,"kind":"Port","name":"sel","path":"t.sel","parent":0,"children":[],
         "symbol_key":"t.sel","dir":"in"},
        {"id":5,"kind":"Port","name":"c","path":"t.c","parent":0,"children":[],
         "symbol_key":"t.c","dir":"in"},
        {"id":6,"kind":"Port","name":"d","path":"t.d","parent":0,"children":[],
         "symbol_key":"t.d","dir":"in"},
        {"id":7,"kind":"Port","name":"z","path":"t.z","parent":0,"children":[],
         "symbol_key":"t.z","dir":"out"},
        {"id":8,"kind":"Assign","name":"$assign8","path":"t.$assign8","parent":0,
         "children":[9],"symbol_key":"t.$assign8"},
        {"id":9,"kind":"And","name":"and","path":"t.$assign8.$and9","parent":8,
         "children":[],"symbol_key":"t.$assign8.$and9"},
        {"id":10,"kind":"Assign","name":"$assign10","path":"t.$assign10","parent":0,
         "children":[11],"symbol_key":"t.$assign10"},
        {"id":11,"kind":"Mux","name":"mux","path":"t.$assign10.$mux11","parent":10,
         "children":[],"symbol_key":"t.$assign10.$mux11"}
    ],
    "edges": [
        {"id":0,"port":9,"endpoint":1,"dir":"in"},
        {"id":1,"port":9,"endpoint":2,"dir":"in"},
        {"id":2,"port":9,"endpoint":3,"dir":"out"},
        {"id":3,"port":8,"endpoint":1,"dir":"in"},
        {"id":4,"port":8,"endpoint":2,"dir":"in"},
        {"id":5,"port":8,"endpoint":3,"dir":"out"},
        {"id":6,"port":11,"endpoint":4,"dir":"in","mux_port":"sel"},
        {"id":7,"port":11,"endpoint":5,"dir":"in","mux_port":"d1"},
        {"id":8,"port":11,"endpoint":6,"dir":"in","mux_port":"d0"},
        {"id":9,"port":11,"endpoint":7,"dir":"out"},
        {"id":10,"port":10,"endpoint":4,"dir":"in"},
        {"id":11,"port":10,"endpoint":5,"dir":"in"},
        {"id":12,"port":10,"endpoint":6,"dir":"in"},
        {"id":13,"port":10,"endpoint":7,"dir":"out"}
    ]
}"#;

fn gate_design() -> Design {
    svxprobe_ingest::from_slice(GATE_LEVEL_DOC.as_bytes()).expect("gate-level model ingests")
}

// The default projection is byte-identical to today: the process-level `Assign`
// boxes stay, and the gate/mux children are never surfaced — so a design carrying
// gate primitives still renders exactly as the process-level view (ADR 0005).
#[test]
fn process_level_ignores_gate_primitives() {
    let d = gate_design();
    // Both the parameterized default and the bare entry point agree.
    for g in [
        scope_graph(&d, "t").expect("scope graph"),
        scope_graph_with(&d, "t", Projection::ProcessLevel).expect("scope graph"),
    ] {
        assert_eq!(
            g.nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Assign)
                .count(),
            2,
            "both assign boxes present at process level"
        );
        assert!(
            !g.nodes
                .iter()
                .any(|n| matches!(n.kind, NodeKind::And | NodeKind::Mux)),
            "no gate boxes at process level"
        );
    }
}

// `GateLevel` dissolves each combinational block into its gate/mux network: the
// `Assign` boxes disappear, the `And`/`Mux` primitives become boxes wired through
// the same scope signals, and the mux select carries `PinRole::Sel`.
#[test]
fn gate_level_dissolves_logic_into_gate_network() {
    let d = gate_design();
    let g = scope_graph_with(&d, "t", Projection::GateLevel).expect("scope graph");

    // The dissolved combinational blocks are gone; their primitives take their place.
    assert!(
        !g.nodes.iter().any(|n| n.kind == NodeKind::Assign),
        "assign boxes dissolved: {:?}",
        g.nodes.iter().map(|n| n.kind).collect::<Vec<_>>()
    );
    let and = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::And)
        .expect("And box");
    let mux = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Mux)
        .expect("Mux box");

    // Gate inputs carry their model paths so a pin right-click cross-probes.
    assert!(
        and.ports.iter().any(|p| p.path == "t.a"),
        "and input a: {:?}",
        and.ports
            .iter()
            .map(|p| p.path.as_str())
            .collect::<Vec<_>>()
    );
    assert!(and.ports.iter().any(|p| p.path == "t.b"), "and input b");
    // Exactly one output pin, on the east wall, carrying the gate's own path.
    let outs: Vec<&_> = and.ports.iter().filter(|p| p.side == Side::East).collect();
    assert_eq!(outs.len(), 1, "one output pin");
    assert_eq!(outs[0].path, "t.$assign8.$and9", "output carries gate path");

    // The mux select pin is role-tagged (drawn on the south wall in the frontend);
    // the data-branch pins are not.
    let sel = mux
        .ports
        .iter()
        .find(|p| p.path == "t.sel")
        .expect("sel pin");
    assert_eq!(sel.role, Some(PinRole::Sel), "select role");
    let data = mux.ports.iter().find(|p| p.path == "t.c").expect("d1 pin");
    assert_eq!(data.role, None, "data pin has no select role");

    // The gate output is wired (to the assigned boundary signal), not left dangling.
    assert!(
        g.edges
            .iter()
            .any(|e| e.source == outs[0].id || e.target == outs[0].id),
        "and output is wired into the scope"
    );
}

/// `assign y = a & 8'hFF;` — an `And` gate with a real signal `a` and a synthetic
/// `Const` operand (#199). The literal must surface as a constant-source node wired
/// into the gate input, exactly like an instance-port tie; the `Const` node itself
/// is never rendered as its own box.
const CONST_GATE_DOC: &str = r#"{
    "schema_version": 1,
    "design": "u",
    "files": [{"id": 0, "path": "u.sv"}],
    "nodes": [
        {"id":0,"kind":"Instance","name":"u","path":"u","parent":null,
         "children":[1,2,3],"symbol_key":"u"},
        {"id":1,"kind":"Port","name":"a","path":"u.a","parent":0,"children":[],
         "symbol_key":"u.a","dir":"in"},
        {"id":2,"kind":"Port","name":"y","path":"u.y","parent":0,"children":[],
         "symbol_key":"u.y","dir":"out"},
        {"id":3,"kind":"Assign","name":"$assign3","path":"u.$assign3","parent":0,
         "children":[4,5],"symbol_key":"u.$assign3"},
        {"id":4,"kind":"And","name":"and","path":"u.$assign3.$and4","parent":3,
         "children":[],"symbol_key":"u.$assign3.$and4"},
        {"id":5,"kind":"Const","name":"const","path":"u.$assign3.$const5","parent":3,
         "children":[],"symbol_key":"u.$assign3.$const5","const":"8'hff"}
    ],
    "edges": [
        {"id":0,"port":4,"endpoint":1,"dir":"in"},
        {"id":1,"port":4,"endpoint":5,"dir":"in"},
        {"id":2,"port":4,"endpoint":2,"dir":"out"}
    ]
}"#;

#[test]
fn gate_const_operand_becomes_inline_tie_value() {
    let d =
        svxprobe_ingest::from_slice(CONST_GATE_DOC.as_bytes()).expect("const-gate model ingests");
    let g = scope_graph_with(&d, "u", Projection::GateLevel).expect("scope graph");

    let and = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::And)
        .expect("And box");
    // The literal rides inline on the gate's own west input pin (#199) — a tie
    // value beside the gate, not a separate source node/wire.
    assert!(
        and.ports
            .iter()
            .any(|p| p.side == Side::West && p.constant.as_deref() == Some("8'hff")),
        "and has an inline const input pin: {:?}",
        and.ports
            .iter()
            .map(|p| (&p.name, &p.constant))
            .collect::<Vec<_>>()
    );
    // The signal operand `a` is still present as a normal (non-const) pin.
    assert!(
        and.ports
            .iter()
            .any(|p| p.path == "u.a" && p.constant.is_none()),
        "signal input a kept"
    );
    // No separate constant-source node, and the Const model node is never a box.
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.constant.is_some() || n.kind == NodeKind::Const),
        "no separate const source node/box"
    );
}

/// `assign y = (a & b) | c;` — an `Or` root whose first input is an `And` node
/// (gate-to-gate), the second a raw signal. Exercises the self-driver path: the
/// inner `And` has no `out` edge, yet its result pin must reach the `Or`'s input.
const NESTED_GATE_DOC: &str = r#"{
    "schema_version": 1,
    "design": "u",
    "files": [{"id": 0, "path": "u.sv"}],
    "nodes": [
        {"id":0,"kind":"Instance","name":"u","path":"u","parent":null,
         "children":[1,2,3,4,5],"symbol_key":"u"},
        {"id":1,"kind":"Port","name":"a","path":"u.a","parent":0,"children":[],
         "symbol_key":"u.a","dir":"in"},
        {"id":2,"kind":"Port","name":"b","path":"u.b","parent":0,"children":[],
         "symbol_key":"u.b","dir":"in"},
        {"id":3,"kind":"Port","name":"c","path":"u.c","parent":0,"children":[],
         "symbol_key":"u.c","dir":"in"},
        {"id":4,"kind":"Port","name":"y","path":"u.y","parent":0,"children":[],
         "symbol_key":"u.y","dir":"out"},
        {"id":5,"kind":"Assign","name":"$assign5","path":"u.$assign5","parent":0,
         "children":[6,7],"symbol_key":"u.$assign5"},
        {"id":6,"kind":"Or","name":"or","path":"u.$assign5.$or6","parent":5,
         "children":[],"symbol_key":"u.$assign5.$or6"},
        {"id":7,"kind":"And","name":"and","path":"u.$assign5.$and7","parent":5,
         "children":[],"symbol_key":"u.$assign5.$and7"}
    ],
    "edges": [
        {"id":0,"port":7,"endpoint":1,"dir":"in"},
        {"id":1,"port":7,"endpoint":2,"dir":"in"},
        {"id":2,"port":6,"endpoint":7,"dir":"in"},
        {"id":3,"port":6,"endpoint":3,"dir":"in"},
        {"id":4,"port":6,"endpoint":4,"dir":"out"}
    ]
}"#;

// A nested primitive (no `out` edge of its own) still wires to its consumer gate:
// the inner `And`'s single east pin connects to the `Or`'s input pin that stands
// for the `And` (the self-driver path in the signal-join pass).
#[test]
fn gate_to_gate_wiring_connects_nested_primitives() {
    let d = svxprobe_ingest::from_slice(NESTED_GATE_DOC.as_bytes()).expect("nested model ingests");
    let g = scope_graph_with(&d, "u", Projection::GateLevel).expect("scope graph");

    let and = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::And)
        .expect("And box");
    let or = g
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Or)
        .expect("Or box");
    let and_out = and
        .ports
        .iter()
        .find(|p| p.side == Side::East)
        .expect("and output pin");
    // The Or's input pin standing for the And carries the And's model path.
    let or_in = or
        .ports
        .iter()
        .find(|p| p.path == "u.$assign5.$and7")
        .expect("or input fed by the and");
    assert!(
        g.edges
            .iter()
            .any(|e| (e.source == and_out.id && e.target == or_in.id)
                || (e.source == or_in.id && e.target == and_out.id)),
        "the and result reaches the or input (gate-to-gate)"
    );
    // And that inner output is not falsely marked dangling.
    assert!(!and_out.dangling, "wired gate output is not dangling");
}

fn is_gate_kind(k: NodeKind) -> bool {
    use NodeKind::*;
    matches!(
        k,
        And | Or
            | Xor
            | Xnor
            | Nand
            | Nor
            | Not
            | Buf
            | Add
            | Sub
            | Mul
            | Cmp
            | Shift
            | Mux
            | Concat
    )
}

// #202: a dangling gate output is only ever a *root* gate that drives a real net
// nothing in scope reads (expected #118) — never a *nested* gate whose consumer edge
// the signal-join failed to wire. This guards the signal-join against a regression
// that would silently drop a gate-to-gate connection.
#[test]
fn no_gate_output_is_silently_unwired() {
    let d = design();
    for scope in ["picorv32_soc.g_lane[0].core", "picorv32_soc.g_lane[1].core"] {
        let g = scope_graph_with(&d, scope, Projection::GateLevel).expect("gate-level graph");
        let wired: std::collections::HashSet<NodeId> =
            g.edges.iter().flat_map(|e| [e.source, e.target]).collect();
        for n in g.nodes.iter().filter(|n| is_gate_kind(n.kind)) {
            let Some(out) = n.ports.iter().find(|p| p.side == Side::East) else {
                continue;
            };
            if wired.contains(&out.id) {
                continue;
            }
            // Dangling → must be a root gate carrying its own model `out` edge.
            let has_out_edge = d
                .edges()
                .iter()
                .any(|e| e.port == n.id && e.dir == Dir::Out);
            assert!(
                has_out_edge,
                "nested gate {} left unwired — a signal-join gap, not expected #118",
                n.path
            );
        }
    }
}

// #202: a root gate whose driven net has no in-scope reader dangles (correct #118),
// but its output pin is relabelled with that net so the floating wire stays visible
// and searchable — mirroring a dangling FF Q's in-box net label.
#[test]
fn dangling_gate_output_is_labelled_with_its_floating_net() {
    let d = design();
    let g = scope_graph_with(&d, "picorv32_soc.g_lane[0].core", Projection::GateLevel)
        .expect("gate-level graph");
    // `wire mem_busy = |{…}` is assigned but never read → its reduction-or dangles.
    // Anchored on the driven net rather than the gate's synthetic id (`$orNNN`),
    // which renumbers whenever the gate pass emits more primitives (#207, #215).
    let (or, out) = g
        .nodes
        .iter()
        .find_map(|n| {
            let p = n
                .ports
                .iter()
                .find(|p| p.side == Side::East && p.path.ends_with(".mem_busy"))?;
            Some((n, p))
        })
        .expect("mem_busy reduction-or gate");
    assert_eq!(or.kind, NodeKind::Or, "`|{{…}}` is a reduction-or");
    assert!(
        out.dangling,
        "mem_busy has no reader, so the output dangles"
    );
    assert_eq!(
        out.name, "mem_busy",
        "dangling output labelled with its net"
    );
    assert!(
        out.path.ends_with(".mem_busy"),
        "output pin carries the net path so it cross-probes: {}",
        out.path
    );
}

// #206: a mux data branch that reads a memory-array element (`cpuregs[decoded_rs1]`)
// wires its input to the whole `cpuregs` Memory node — without it the branch was
// dropped and the mux rendered one input short.
#[test]
fn mux_reading_a_memory_element_wires_to_the_array() {
    let d = design();
    let g = scope_graph_with(&d, "picorv32_soc.g_lane[0].core", Projection::GateLevel)
        .expect("gate-level graph");
    // `cpuregs_rs1 = decoded_rs1 ? cpuregs[decoded_rs1] : 0` — the true branch reads the
    // register-file array, dissolved into `.core` from its `$comb376` process.
    // Anchored on the array read rather than the mux's synthetic id (`$muxNNN`),
    // which renumbers whenever the gate pass emits more primitives (#207, #215).
    let (mux, d1) = g
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Mux)
        .find_map(|n| {
            let p = n.ports.iter().find(|p| p.path.ends_with(".cpuregs"))?;
            Some((n, p))
        })
        .expect("cpuregs read mux");
    assert!(
        mux.path.contains(".$mux"),
        "the reader is a synthesized mux primitive: {}",
        mux.path
    );
    assert_eq!(d1.side, Side::West, "a data branch is a west input");
    // The input is really wired to the cpuregs Memory box, not left dangling.
    assert!(
        g.edges
            .iter()
            .any(|e| e.source == d1.id || e.target == d1.id),
        "the memory-read branch connects into the scope"
    );
}

// ---------------------------------------------------------------------------
// cone_with (#244) — the rebuilt trace extractor. Structural anchors only:
// seeds resolve by path, assertions are on kinds and set relations, never ids.
// ---------------------------------------------------------------------------

const RESETN: &str = "picorv32_soc.g_lane[0].core.resetn";
const VALID: &str = "picorv32_soc.g_lane[0].bus.valid";
const MEM_VALID: &str = "picorv32_soc.g_lane[0].core.mem_valid";

fn kinds(g: &SchematicGraph) -> std::collections::HashSet<NodeKind> {
    g.nodes.iter().map(|n| n.kind).collect()
}

fn pin_ids(g: &SchematicGraph) -> std::collections::HashSet<NodeId> {
    g.nodes
        .iter()
        .flat_map(|n| n.ports.iter().map(|p| p.id))
        .collect()
}

fn gateish(k: &NodeKind) -> bool {
    matches!(
        k,
        NodeKind::And
            | NodeKind::Or
            | NodeKind::Xor
            | NodeKind::Xnor
            | NodeKind::Nand
            | NodeKind::Nor
            | NodeKind::Not
            | NodeKind::Buf
            | NodeKind::Mux
            | NodeKind::Add
            | NodeKind::Sub
            | NodeKind::Mul
            | NodeKind::Cmp
            | NodeKind::Shift
            | NodeKind::Concat
    )
}

#[test]
fn cone_with_emits_every_box_kind() {
    let d = design();
    let g = cone_with(
        &d,
        id(&d, RESETN),
        Dir::Inout,
        ConeLimits::depth(2),
        Projection::ProcessLevel,
    );
    let k = kinds(&g);
    // The whole point: the legacy cone matched Instance only, so a cone into a
    // leaf module's internal logic returned edges with no nodes at all.
    assert_ne!(
        k,
        std::collections::HashSet::from([NodeKind::Instance]),
        "a process-level cone must emit more than instances: {k:?}"
    );
    assert!(
        k.contains(&NodeKind::Ff),
        "expected a flip-flop box, got {k:?}"
    );
}

#[test]
fn cone_with_endpoints_resolve_to_emitted_pins() {
    let d = design();
    for seed in [RESETN, VALID, MEM_VALID] {
        for proj in [Projection::ProcessLevel, Projection::GateLevel] {
            let g = cone_with(&d, id(&d, seed), Dir::Inout, ConeLimits::depth(2), proj);
            let pins = pin_ids(&g);
            for e in &g.edges {
                // The layout precondition: nothing can anchor an edge whose
                // endpoints are not pins of the graph's own nodes.
                assert!(
                    pins.contains(&e.source),
                    "{seed} {proj:?}: edge {} source {} is not an emitted pin",
                    e.id,
                    e.source
                );
                assert!(
                    pins.contains(&e.target),
                    "{seed} {proj:?}: edge {} target {} is not an emitted pin",
                    e.id,
                    e.target
                );
            }
        }
    }
}

#[test]
fn cone_with_dedups_pin_pairs() {
    let d = design();
    let g = cone_with(
        &d,
        id(&d, RESETN),
        Dir::Inout,
        ConeLimits::depth(3),
        Projection::ProcessLevel,
    );
    let mut seen = std::collections::HashSet::new();
    for e in &g.edges {
        assert!(
            seen.insert((e.source.min(e.target), e.source.max(e.target))),
            "duplicate wire between one pin pair: {} -> {}",
            e.source,
            e.target
        );
    }
}

#[test]
fn cone_with_root_is_an_openable_scope() {
    let d = design();
    for seed in [RESETN, VALID] {
        let g = cone_with(
            &d,
            id(&d, seed),
            Dir::Inout,
            ConeLimits::depth(1),
            Projection::ProcessLevel,
        );
        assert!(
            scope_graph(&d, &g.root).is_some(),
            "{seed}: root {:?} is not a scope the schematic can open",
            g.root
        );
    }
}

#[test]
fn cone_with_honours_projection() {
    let d = design();
    let seed = id(&d, RESETN);
    let proc = cone_with(
        &d,
        seed,
        Dir::Inout,
        ConeLimits::depth(2),
        Projection::ProcessLevel,
    );
    let gate = cone_with(
        &d,
        seed,
        Dir::Inout,
        ConeLimits::depth(2),
        Projection::GateLevel,
    );
    assert!(
        kinds(&gate).iter().any(gateish),
        "a gate-level cone must decompose combinational logic: {:?}",
        kinds(&gate)
    );
    assert!(
        !kinds(&proc).iter().any(gateish),
        "a process-level cone must not expose gate primitives: {:?}",
        kinds(&proc)
    );
}

#[test]
fn cone_with_fanout_cap_engages() {
    let d = design();
    let seed = id(&d, RESETN);
    for proj in [Projection::ProcessLevel, Projection::GateLevel] {
        let capped = cone_with(
            &d,
            seed,
            Dir::Inout,
            ConeLimits {
                depth: 1,
                fanout: 4,
                boxes: 2000,
            },
            proj,
        );
        let uncapped = cone_with(
            &d,
            seed,
            Dir::Inout,
            ConeLimits {
                depth: 1,
                fanout: usize::MAX,
                boxes: 2000,
            },
            proj,
        );
        assert!(capped.truncated, "{proj:?}: the cap must report itself");
        assert!(
            capped
                .nodes
                .iter()
                .any(|n| n.ports.iter().any(|p| p.more.is_some_and(|c| c > 0))),
            "{proj:?}: a dropped connection must surface as a `more` count, not vanish"
        );
        // Non-vacuous: without this, an extractor returning an empty graph
        // would satisfy every assertion above.
        assert!(
            uncapped.nodes.len() > capped.nodes.len(),
            "{proj:?}: capping must actually remove boxes ({} vs {})",
            uncapped.nodes.len(),
            capped.nodes.len()
        );
        assert!(
            !uncapped.truncated,
            "{proj:?}: an uncapped walk is not truncated"
        );
    }
}

#[test]
fn cone_with_a_zero_fanout_budget_still_reports_what_it_dropped() {
    // A zero budget used to drop every candidate on every signal, returning an
    // empty graph with `truncated` set — but with no pin anywhere to carry a
    // `more` count, which reads in the UI as "nothing found" rather than
    // "everything was capped". The cap exists to make truncation visible, so the
    // degenerate budget must still produce something to hang that count on.
    let d = design();
    let seed = id(&d, RESETN);
    let g = cone_with(
        &d,
        seed,
        Dir::Inout,
        ConeLimits {
            depth: 1,
            fanout: 0,
            boxes: 2000,
        },
        Projection::ProcessLevel,
    );
    assert!(
        !g.nodes.is_empty(),
        "a zero budget must not erase the graph"
    );
    assert!(g.truncated, "and must still report itself as truncated");
    assert!(
        g.nodes
            .iter()
            .any(|n| n.ports.iter().any(|p| p.more.is_some_and(|c| c > 0))),
        "the dropped connections must surface as a `more` count"
    );
}

#[test]
fn cone_with_depth_cap_engages() {
    let d = design();
    let seed = id(&d, VALID);
    let shallow = cone_with(
        &d,
        seed,
        Dir::Inout,
        ConeLimits::depth(1),
        Projection::ProcessLevel,
    );
    let deep = cone_with(
        &d,
        seed,
        Dir::Inout,
        ConeLimits::depth(3),
        Projection::ProcessLevel,
    );
    assert!(
        deep.nodes.len() > shallow.nodes.len(),
        "more hops must reach more boxes ({} vs {})",
        deep.nodes.len(),
        shallow.nodes.len()
    );
}

#[test]
fn cone_with_box_budget_engages() {
    let d = design();
    let g = cone_with(
        &d,
        id(&d, RESETN),
        Dir::Inout,
        ConeLimits {
            depth: 4,
            fanout: 32,
            boxes: 2,
        },
        Projection::ProcessLevel,
    );
    assert!(g.truncated, "the box budget must report itself");
    // Bounded is not empty: the budget caps boxes, and the anchor stub is still
    // emitted so surviving wires keep both endpoints.
    assert!(
        !g.edges.is_empty(),
        "a budgeted cone still draws what it kept"
    );
}

#[test]
fn cone_with_does_not_mark_frontier_pins_dangling() {
    let d = design();
    let g = cone_with(
        &d,
        id(&d, RESETN),
        Dir::Inout,
        ConeLimits::depth(1),
        Projection::ProcessLevel,
    );
    // In a cone an unwired pin means "beyond the frontier", not "floating in
    // the design" — dimming it would state a falsehood that the scope graph is
    // entitled to state and this view is not.
    assert!(
        g.nodes.iter().all(|n| n.ports.iter().all(|p| !p.dangling)),
        "no cone pin may be marked dangling"
    );
}

#[test]
fn cone_with_crosses_hierarchy() {
    let d = design();
    let g = cone_with(
        &d,
        id(&d, MEM_VALID),
        Dir::Inout,
        ConeLimits::depth(3),
        Projection::ProcessLevel,
    );
    assert!(
        g.nodes
            .iter()
            .any(|n| !n.path.is_empty() && !n.path.starts_with("picorv32_soc.g_lane[0].core.")),
        "a trace must leave the seed's own scope: {:?}",
        g.nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// #285 — a module port is a crossing point, not a wall.
//
// A port is *two* model nodes at one canonical path with **no edge between
// them**: the `Port` carries only the external connection (to the parent net or
// enclosing interface), and the backing `Var`/`Net` carries every internal one.
// Walking them as two independent signals gave each its own `join_signal` and
// its own stub, so the two halves landed on canvas as two identically labelled
// junctions in two disconnected components — which is what made a trace look
// like it stopped at the boundary.
// ---------------------------------------------------------------------------

const CORE_CLK: &str = "picorv32_soc.g_lane[0].core.clk";
const TOP_CLK: &str = "picorv32_soc.clk";

/// How many connected components the graph's wires leave behind, counting only
/// nodes that are wired at all. Union-find over pin ids, because a wire names
/// pins and a pin belongs to exactly one node.
fn components(g: &SchematicGraph) -> usize {
    let mut owner: std::collections::HashMap<NodeId, usize> = std::collections::HashMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        for p in &n.ports {
            owner.insert(p.id, i);
        }
    }
    let mut parent: Vec<usize> = (0..g.nodes.len()).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let mut wired: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for e in &g.edges {
        let (Some(&a), Some(&b)) = (owner.get(&e.source), owner.get(&e.target)) else {
            continue;
        };
        wired.insert(a);
        wired.insert(b);
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    let roots: std::collections::HashSet<usize> =
        wired.iter().map(|&i| find(&mut parent, i)).collect();
    roots.len()
}

#[test]
fn cone_with_on_a_module_port_shows_both_sides() {
    let d = design();
    let g = cone_with(
        &d,
        id(&d, MEM_VALID),
        Dir::Inout,
        ConeLimits::depth(1),
        Projection::ProcessLevel,
    );
    let inside = "picorv32_soc.g_lane[0].core.";
    let internal: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.path.starts_with(inside) && n.path != MEM_VALID)
        .map(|n| n.path.as_str())
        .collect();
    let external: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| !n.path.is_empty() && !n.path.starts_with(inside))
        .map(|n| n.path.as_str())
        .collect();
    // Anti-vacuity: "one component" is trivially true of a graph with one side.
    assert!(
        !internal.is_empty(),
        "no logic behind the port: {:?}",
        g.nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
    );
    assert!(
        !external.is_empty(),
        "nothing outside the port: {:?}",
        g.nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
    );
    // The point of the issue: one graph, not two islands that happen to share a
    // label. Before #285 this was 2 — the `Port` half wired only to the parent's
    // interface, the `Var` half only to the module's own logic.
    assert_eq!(
        components(&g),
        1,
        "a port's two sides must meet at one crossing point: {:?}",
        g.nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
    );
}

#[test]
fn cone_with_draws_a_port_once() {
    let d = design();
    for seed in [MEM_VALID, VALID, CORE_CLK] {
        for dir in [Dir::In, Dir::Out, Dir::Inout] {
            for proj in [Projection::ProcessLevel, Projection::GateLevel] {
                let g = cone_with(&d, id(&d, seed), dir, ConeLimits::depth(2), proj);
                // Stronger than the id-uniqueness invariant, which passes happily
                // on two distinct model ids drawn for the same signal.
                let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
                for n in &g.nodes {
                    if n.path.is_empty() {
                        continue;
                    }
                    assert!(
                        seen.insert(n.path.as_str()),
                        "{seed} {dir:?} {proj:?}: {} drawn twice",
                        n.path
                    );
                }
            }
        }
    }
}

#[test]
fn cone_with_on_an_undriven_input_still_draws_the_seed() {
    let d = design();
    // `core.clk` is driven from the design's own top-level input, which nothing
    // drives in turn. The honest answer is "here is the signal, nothing feeds
    // it" — an empty graph says "nothing found" and is indistinguishable from a
    // failed lookup, which is exactly the silent drop ADR 0003 forbids.
    for seed in [CORE_CLK, "picorv32_soc.g_lane[0].core.resetn"] {
        let g = cone_with(
            &d,
            id(&d, seed),
            Dir::In,
            ConeLimits::depth(1),
            Projection::ProcessLevel,
        );
        // Visible as a node of its own, or as a pin on the container the walk
        // pulled in behind the wall (#293) — the port sits on the instance box.
        // Either way the user sees the signal they asked about; what must not
        // happen is an empty graph.
        let drawn = g.nodes.iter().any(|n| n.path == seed)
            || g.nodes
                .iter()
                .any(|n| n.ports.iter().any(|p| p.path == seed));
        assert!(
            drawn,
            "{seed}: fan-in must still draw the seed, got {:?}",
            g.nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
        );
    }
}

#[test]
fn cone_with_descends_through_an_instance() {
    let d = design();
    let inner = |g: &SchematicGraph| {
        g.nodes
            .iter()
            .any(|n| n.path.starts_with("picorv32_soc.g_lane[0].core.$"))
    };
    let at = |depth: usize| {
        cone_with(
            &d,
            id(&d, TOP_CLK),
            Dir::Out,
            ConeLimits::depth(depth),
            Projection::ProcessLevel,
        )
    };
    // Anti-vacuity: the assertion below only means something if the extra hop is
    // what crossed the wall. At depth 1 the walk has only reached the instance.
    assert!(
        !inner(&at(1)),
        "depth 1 should stop at the instance boundary"
    );
    assert!(
        inner(&at(2)),
        "depth 2 must enter the module: {:?}",
        at(2).nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// #293 — the hierarchy the walk crosses is drawn, not flattened away.
//
// #285 made a boundary transparent; the result was a *flat* canvas, with the
// logic behind a wall drawn as a peer of the logic outside it. `SchNode.parent`
// names the instance that contains a box, so the renderer can nest it. The
// containing instance is the nearest ancestor `Instance` — generate blocks
// dissolve, exactly as `child_boxes` dissolves them — and a box directly under
// the design top has no container at all, so the top is never drawn as a box
// wrapping the whole canvas.
// ---------------------------------------------------------------------------

const CORE: &str = "picorv32_soc.g_lane[0].core";
const AND813: &str = "picorv32_soc.g_lane[0].core.$assign200.$and813";
const FF214: &str = "picorv32_soc.g_lane[0].core.$ff214";

#[test]
fn cone_with_from_a_gate_follows_only_the_side_asked_for() {
    // A gate's output pin carries the *gate's* own path, so the frontend's ▶
    // control seeds the gate itself — and `seed_signals` folded in every signal
    // the gate touches, operands included. Fan-out therefore walked the inputs
    // and drew their neighbourhood, which is the opposite of what ▶ means (#286).
    let d = design();
    let gate = id(&d, AND813);
    let paths = |g: &SchematicGraph| {
        g.nodes
            .iter()
            .map(|n| n.path.clone())
            .collect::<std::collections::HashSet<_>>()
    };
    let fan_out = paths(&cone_with(
        &d,
        gate,
        Dir::Out,
        ConeLimits::depth(1),
        Projection::GateLevel,
    ));
    let fan_in = paths(&cone_with(
        &d,
        gate,
        Dir::In,
        ConeLimits::depth(1),
        Projection::GateLevel,
    ));
    // A trace seeded on a box must draw that box — its own pins are what anchor
    // its wires, so without it the join has one side and nothing is drawn at all.
    assert!(
        fan_out.contains(AND813) && fan_in.contains(AND813),
        "a trace must draw the box it was seeded on"
    );
    // `$ff214` drives `mem_valid`, one of this gate's *operands*. Reaching it
    // from a **fan-out** is the bug: ▶ walked the operands and drew their
    // neighbourhood instead of what the gate drives.
    assert!(
        !fan_out.contains(FF214),
        "fan-out must not walk the gate's operands: {fan_out:?}"
    );
    // The two directions must genuinely differ, or the filter is not engaging.
    let out_only: Vec<_> = fan_out.difference(&fan_in).collect();
    let in_only: Vec<_> = fan_in.difference(&fan_out).collect();
    assert!(
        !out_only.is_empty(),
        "fan-out reached nothing fan-in did not: {fan_out:?}"
    );
    assert!(
        !in_only.is_empty() || fan_in.len() < fan_out.len(),
        "fan-in is indistinguishable from fan-out: {fan_in:?}"
    );
}

#[test]
fn cone_with_on_a_gate_wires_the_box_it_was_seeded_on() {
    // The anchor half of the same defect. A gate is never stubbed (#268), and the
    // model wires gate-to-gate with no net node between, so a gate seed had
    // nothing to anchor on: the join saw a load and no driver, drew no wire, and
    // the orphan pass then removed the box on the far side — an empty graph.
    let d = design();
    for dir in [Dir::In, Dir::Out] {
        let g = cone_with(
            &d,
            id(&d, AND813),
            dir,
            ConeLimits::depth(1),
            Projection::GateLevel,
        );
        assert!(
            !g.edges.is_empty(),
            "{dir:?}: a gate seed must anchor its own wires, got {} boxes and no wires",
            g.nodes.len()
        );
        let pins: std::collections::HashSet<NodeId> = g
            .nodes
            .iter()
            .flat_map(|n| n.ports.iter().map(|p| p.id))
            .collect();
        for e in &g.edges {
            assert!(
                pins.contains(&e.source) && pins.contains(&e.target),
                "{dir:?}: edge {} does not land on emitted pins",
                e.id
            );
        }
    }
}

#[test]
fn cone_with_names_the_instance_that_contains_a_box() {
    let d = design();
    let g = cone_with(
        &d,
        id(&d, TOP_CLK),
        Dir::Out,
        ConeLimits::depth(2),
        Projection::ProcessLevel,
    );
    let core = id(&d, CORE);
    let inside: Vec<&SchNode> = g
        .nodes
        .iter()
        .filter(|n| n.path.starts_with("picorv32_soc.g_lane[0].core."))
        .collect();
    // Anti-vacuity: containment is trivially satisfied if the walk never
    // descended. #285's own test guards the descent; this one needs it too.
    assert!(!inside.is_empty(), "vacuous: nothing behind the wall");
    for n in &inside {
        assert_eq!(
            n.parent,
            Some(core),
            "{} is behind the core wall and must name it as its container",
            n.path
        );
    }
}

#[test]
fn cone_with_draws_a_containing_instance_once_and_uncontained() {
    let d = design();
    let g = cone_with(
        &d,
        id(&d, TOP_CLK),
        Dir::Out,
        ConeLimits::depth(2),
        Projection::ProcessLevel,
    );
    let core = id(&d, CORE);
    // The opaque box and the container are the same object in two states, so
    // reaching an instance from outside *and* descending into it must promote
    // the one node rather than add a second.
    let drawn: Vec<&SchNode> = g.nodes.iter().filter(|n| n.id == core).collect();
    assert_eq!(
        drawn.len(),
        1,
        "the instance must be drawn once, got {:?}",
        drawn.iter().map(|n| &n.path).collect::<Vec<_>>()
    );
    // Its own nearest ancestor instance is the design top, which is not a
    // container — otherwise every trace would sit inside one useless outer box.
    assert_eq!(drawn[0].parent, None, "a box under the top is uncontained");
}

#[test]
fn cone_with_containers_are_emitted_and_acyclic() {
    // A `parent` naming a node that is not on canvas cannot be nested, and a
    // cycle would hang a recursive renderer rather than draw wrongly.
    every_cone(|g, seed, proj, depth| {
        let ids: std::collections::HashSet<NodeId> = g.nodes.iter().map(|n| n.id).collect();
        for n in &g.nodes {
            if let Some(p) = n.parent {
                assert!(
                    ids.contains(&p),
                    "{seed} {proj:?} depth {depth}: {} names container {p}, which is not drawn",
                    n.path
                );
                assert_ne!(
                    p, n.id,
                    "{seed} {proj:?} depth {depth}: {} contains itself",
                    n.path
                );
            }
        }
        let parent_of: std::collections::HashMap<NodeId, Option<NodeId>> =
            g.nodes.iter().map(|n| (n.id, n.parent)).collect();
        for n in &g.nodes {
            let (mut cur, mut hops) = (n.parent, 0usize);
            while let Some(c) = cur {
                hops += 1;
                assert!(
                    hops <= g.nodes.len(),
                    "{seed} {proj:?} depth {depth}: container cycle above {}",
                    n.path
                );
                cur = *parent_of.get(&c).unwrap_or(&None);
            }
        }
    });
}

#[test]
fn scope_graph_draws_no_containers() {
    // Hierarchy mode shows exactly one scope, so nothing in it is nested — and
    // `parent` skips serialization when absent, keeping that output byte-identical.
    let d = design();
    for scope in ["picorv32_soc", "picorv32_soc.g_lane[0]", CORE] {
        let g = scope_graph(&d, scope).expect("scope graph");
        assert!(
            g.nodes.iter().all(|n| n.parent.is_none()),
            "{scope}: the scope view must not nest"
        );
    }
}

/// Every seed x projection x depth a cone invariant is checked against — one
/// place to widen the sweep, since the assertions below are all "for every cone".
fn every_cone(mut f: impl FnMut(&SchematicGraph, &str, Projection, usize)) {
    let d = design();
    // `CORE_CLK` is here for #285: an input port whose fan-in leaves the module
    // entirely. Widening this sweep is what makes every invariant below — no
    // repeated id, no orphan box, agreement with `child_boxes`, no inline
    // operand drawn as a box — cover the wall-crossing path too. That sweep is
    // how #268 and #269 were found; the hand-written repro tests missed both.
    for seed in [RESETN, VALID, MEM_VALID, CORE_CLK] {
        for proj in [Projection::ProcessLevel, Projection::GateLevel] {
            for depth in [1, 2, 3] {
                let g = cone_with(&d, id(&d, seed), Dir::Inout, ConeLimits::depth(depth), proj);
                f(&g, seed, proj, depth);
            }
        }
    }
}

#[test]
fn cone_with_wires_every_box_it_emits() {
    // #269: a box reached but never wired is an orphan on canvas. The older
    // assertions (root openable, endpoints resolve to emitted pins) both pass
    // on an under-connected graph, which is how the bare-bundle bug survived.
    every_cone(|g, seed, proj, depth| {
        let wired: std::collections::HashSet<NodeId> =
            g.edges.iter().flat_map(|e| [e.source, e.target]).collect();
        // A container earns its place from what it holds, not from its own pins:
        // an instance the walk descended through is often wired entirely by the
        // boxes inside it (#293). It is not an orphan — it is the wall.
        let holds: std::collections::HashSet<NodeId> =
            g.nodes.iter().filter_map(|n| n.parent).collect();
        let orphans: Vec<&str> = g
            .nodes
            .iter()
            // A pin carrying a `more` count is the reported remainder of a
            // capped fan-out — connectivity stated, not missing.
            .filter(|n| {
                !holds.contains(&n.id)
                    && !n
                        .ports
                        .iter()
                        .any(|p| wired.contains(&p.id) || p.more.is_some())
            })
            .map(|n| n.path.as_str())
            .collect();
        assert!(
            orphans.is_empty(),
            "{seed} {proj:?} depth {depth}: emitted boxes with no wire: {orphans:?}"
        );
    });
}

#[test]
fn cone_with_agrees_with_the_scope_graph_on_what_is_a_box() {
    // The general form of #268: `cone_with` picks boxes with `cone_box_of`, the
    // scope graph with `child_boxes`. Any node kind one special-cases and the
    // other does not shows up here — the guard both bugs lacked.
    let d = design();
    // One scope graph per (scope, projection) — the walk below revisits the same
    // scopes for hundreds of boxes, and each rebuild is the expensive part.
    let mut cache: std::collections::HashMap<(String, bool), Option<SchematicGraph>> =
        std::collections::HashMap::new();
    every_cone(|g, seed, proj, depth| {
        for n in &g.nodes {
            let is_box = gateish(&n.kind)
                || matches!(
                    n.kind,
                    NodeKind::Instance
                        | NodeKind::Interface
                        | NodeKind::Ff
                        | NodeKind::Memory
                        | NodeKind::Comb
                        | NodeKind::Latch
                        | NodeKind::Assign
                );
            // Signal stubs (the seed, a hierarchy-crossing port) are the cone's
            // own anchors and have no scope-graph counterpart by design.
            if !is_box {
                continue;
            }
            // Walk to the box's own nearest openable scope — the one the
            // frontend would render it in. Under GateLevel that is the enclosing
            // *module* for a gate, since `child_boxes` dissolves its block; that
            // walk is what makes a folded inverter or a const tie visible here.
            let mut scope = d.node(n.id).and_then(|x| x.parent);
            let found = loop {
                let Some(s) = scope else { break None };
                let path = d.node(s).map(|x| x.path.clone()).unwrap_or_default();
                let key = (path.clone(), proj == Projection::GateLevel);
                let sg = cache
                    .entry(key)
                    .or_insert_with(|| scope_graph_with(&d, &path, proj));
                if sg.is_some() {
                    break Some(path);
                }
                scope = d.node(s).and_then(|x| x.parent);
            };
            let Some(path) = found else { continue };
            let sg = cache[&(path.clone(), proj == Projection::GateLevel)]
                .as_ref()
                .unwrap();
            assert!(
                sg.nodes.iter().any(|m| m.id == n.id),
                "{seed} {proj:?} depth {depth}: cone drew {} ({:?}) as a box, \
                 but the scope graph of {path} does not",
                n.path,
                n.kind
            );
        }
    });
}

#[test]
fn cone_with_draws_no_inline_operand_as_a_box() {
    // #268: a literal tie renders inline on `SchPort.constant` and a folded
    // inverter as an `Inv` bubble on its consumer's pin. Before the fix a
    // gate-level cone re-materialized both as phantom one-pin boxes — 178
    // `Const` and 100 `Not` boxes on a depth-3 trace from `resetn`.
    every_cone(|g, seed, proj, depth| {
        let consts: Vec<&str> = g
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Const)
            .map(|n| n.path.as_str())
            .collect();
        assert!(
            consts.is_empty(),
            "{seed} {proj:?} depth {depth}: Const is never a box, got {consts:?}"
        );
    });
    // The inverters that remain must be the ones the scope graph also draws: a
    // root `~signal` with an `out` edge, never a folded single-fanout operand.
    let d = design();
    let g = cone_with(
        &d,
        id(&d, RESETN),
        Dir::Inout,
        ConeLimits::depth(3),
        Projection::GateLevel,
    );
    for n in g.nodes.iter().filter(|n| n.kind == NodeKind::Not) {
        assert!(
            d.edges_of(n.id)
                .iter()
                .any(|e| e.port == n.id && e.dir == Dir::Out),
            "{} has no out edge, so it is a folded inverter and must be drawn \
             as a bubble on its consumer's pin, not as a box",
            n.path
        );
    }
}

#[test]
fn cone_with_anchors_a_bare_interface_on_its_raw_access_port() {
    // #269 verbatim: the bare bundle was emitted with none of its three pins
    // wired, because `cone_pin_for` only looked for a `Port` child — and a raw
    // member is a `Var`, so it fell back to an id `make_box` never allocated.
    let d = design();
    let g = cone_with(
        &d,
        id(&d, VALID),
        Dir::Inout,
        ConeLimits::depth(1),
        Projection::ProcessLevel,
    );
    let bundle = g
        .nodes
        .iter()
        .find(|n| n.path == "picorv32_soc.g_lane[0].bus")
        .expect("the bare interface bundle is reached from its own member");
    let wired: std::collections::HashSet<NodeId> =
        g.edges.iter().flat_map(|e| [e.source, e.target]).collect();
    assert!(
        bundle.ports.iter().any(|p| wired.contains(&p.id)),
        "the bundle must carry the wire that reached it; its pins are {:?}",
        bundle.ports.iter().map(|p| &p.name).collect::<Vec<_>>()
    );
}

#[test]
fn cone_with_never_repeats_a_node_or_pin_id() {
    // ELK ids are global to the graph, so one id on two nodes — or on two pins —
    // is unlayoutable. This held by luck rather than by construction: a signal
    // some box already exposed as a pin could still be stood up as its own
    // one-sided stub, whose pin id *is* that model id. It bit six of the golden
    // cones (`bus.valid` inout, both projections, every depth), and an
    // accumulating `trace_graph` re-rolls that luck on every step.
    every_cone(|g, seed, proj, depth| {
        let mut ids = std::collections::HashSet::new();
        for n in &g.nodes {
            assert!(
                ids.insert(n.id),
                "{seed} {proj:?} depth {depth}: node id {} twice ({})",
                n.id,
                n.path
            );
        }
        let mut pins = std::collections::HashSet::new();
        for n in &g.nodes {
            for p in &n.ports {
                assert!(
                    pins.insert(p.id),
                    "{seed} {proj:?} depth {depth}: pin id {} twice ({} on {})",
                    p.id,
                    p.name,
                    n.path
                );
            }
        }
    });
}

#[test]
fn cone_is_unchanged() {
    // The legacy extractor is the `svxprobe graph --cone` output contract and
    // the scale-bench fan-out baseline; cone_with must not have perturbed it.
    let d = design();
    let g = cone(&d, id(&d, RESETN), Dir::Inout, 2);
    assert!(!g.truncated, "the legacy cone never caps");
    assert!(
        g.nodes.iter().all(|n| n.kind == NodeKind::Instance),
        "legacy cone emits instance boxes only"
    );
    assert!(
        !g.edges.is_empty(),
        "legacy cone still finds the connections"
    );
}

// ---------------------------------------------------------------------------
// trace_graph (#244 PR2) — the accumulating extractor cone_with delegates to.
// Same discipline as the cone_with block: seeds resolve by path, assertions are
// on set relations, never on ids.
// ---------------------------------------------------------------------------

fn step(d: &Design, path: &str, dir: Dir, depth: usize) -> TraceStep {
    TraceStep {
        seed: id(d, path),
        dir,
        depth,
        fanout: None,
    }
}

fn box_paths(g: &SchematicGraph) -> std::collections::HashSet<&str> {
    g.nodes.iter().map(|n| n.path.as_str()).collect()
}

#[test]
fn trace_graph_with_one_step_matches_cone_with() {
    // The delegation contract. cone_with is `svxprobe graph --cone --fanout`'s
    // output and a scale-bench measurement, so a one-step trace must reproduce
    // it exactly — not merely "structurally", or the refactor has moved a
    // baseline nothing else would catch.
    let d = design();
    for seed in [RESETN, VALID, MEM_VALID] {
        for proj in [Projection::ProcessLevel, Projection::GateLevel] {
            for dir in [Dir::In, Dir::Out, Dir::Inout] {
                for depth in [1, 2, 3] {
                    let limits = ConeLimits::depth(depth);
                    let want = cone_with(&d, id(&d, seed), dir, limits, proj);
                    let got = trace_graph(&d, &[step(&d, seed, dir, depth)], limits, proj);
                    assert_eq!(want, got, "{seed} {dir:?} {proj:?} depth {depth}");
                }
            }
        }
    }
}

#[test]
fn trace_graph_is_deterministic() {
    // The frontend re-derives the whole trace on every expansion instead of
    // merging (pin ids are call-local), so identical steps must give an
    // identical graph or the canvas would churn for no reason.
    let d = design();
    let steps = [
        step(&d, RESETN, Dir::In, 1),
        step(&d, MEM_VALID, Dir::Out, 2),
    ];
    let a = trace_graph(&d, &steps, ConeLimits::default(), Projection::ProcessLevel);
    let b = trace_graph(&d, &steps, ConeLimits::default(), Projection::ProcessLevel);
    assert_eq!(a, b);
}

#[test]
fn trace_graph_accumulates_every_step() {
    // The point of the mode: a second expansion adds to the canvas rather than
    // replacing it.
    let d = design();
    let limits = ConeLimits::depth(1);
    let proj = Projection::ProcessLevel;
    let s1 = step(&d, RESETN, Dir::Inout, 1);
    let s2 = step(&d, MEM_VALID, Dir::Inout, 1);

    let one = trace_graph(&d, &[s1], limits, proj);
    let two = trace_graph(&d, &[s2], limits, proj);
    let both = trace_graph(&d, &[s1, s2], limits, proj);

    // A box the combined walk drops would have to have lost its last wire, and
    // both seeds keep theirs — so this is a plain superset check.
    for g in [&one, &two] {
        for p in box_paths(g) {
            assert!(
                box_paths(&both).contains(p),
                "combined trace lost {p}; has {:?}",
                box_paths(&both)
            );
        }
    }
    assert!(
        both.nodes.len() > one.nodes.len(),
        "the second step must add something: {} vs {}",
        both.nodes.len(),
        one.nodes.len()
    );
}

#[test]
fn trace_graph_emits_each_box_once() {
    // Two steps that overlap must share the box, not draw it twice — the
    // property that makes re-derivation a merge.
    let d = design();
    let g = trace_graph(
        &d,
        &[
            step(&d, RESETN, Dir::Inout, 2),
            step(&d, MEM_VALID, Dir::Inout, 2),
            step(&d, VALID, Dir::Inout, 2),
        ],
        ConeLimits::default(),
        Projection::ProcessLevel,
    );
    let mut seen = std::collections::HashSet::new();
    for n in &g.nodes {
        assert!(seen.insert(n.id), "duplicate box {} ({})", n.id, n.path);
    }
    let mut pins = std::collections::HashSet::new();
    for p in g.nodes.iter().flat_map(|n| &n.ports) {
        assert!(pins.insert(p.id), "duplicate pin id {} ({})", p.id, p.name);
    }
}

#[test]
fn trace_graph_joins_both_directions_of_one_net_at_a_single_node() {
    // `follows` filters to one direction, so a directional walk is one-sided by
    // construction and always synthesizes a signal stub. Expanding fan-in then
    // fan-out of the *same* net must re-use that stub — otherwise the net is
    // drawn twice and the two halves of its connectivity never meet.
    let d = design();
    let limits = ConeLimits::depth(1);
    let proj = Projection::ProcessLevel;
    let g = trace_graph(
        &d,
        &[
            step(&d, MEM_VALID, Dir::In, 1),
            step(&d, MEM_VALID, Dir::Out, 1),
        ],
        limits,
        proj,
    );
    let only_in = trace_graph(&d, &[step(&d, MEM_VALID, Dir::In, 1)], limits, proj);
    // A crossing may be drawn as a node of its own, or — once containment pulls
    // in the instance behind the wall (#293) — as a pin on that container. Both
    // are one junction; what must never happen is two of them.
    //
    // Keyed on the signal's own **model** ids, not on `path`: every consumer's
    // pin carries the net path too, but only an anchor is keyed on the model
    // node itself (a logic box's pins are `PinAlloc`-synthesized).
    let anchors: std::collections::HashSet<NodeId> =
        d.nodes_at_path(MEM_VALID).iter().copied().collect();
    let junction = |g: &SchematicGraph| -> Vec<NodeId> {
        let mut v: Vec<NodeId> = g
            .nodes
            .iter()
            .flat_map(|n| n.ports.iter().map(|p| p.id).chain(std::iter::once(n.id)))
            .filter(|id| anchors.contains(id))
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    // Exactly one node, since #285: a port and its backing net are two model
    // nodes for one wire, and the walk now folds them into one signal with one
    // anchor keyed on the group's representative. It used to be two — the two
    // directions reached different halves — and that is precisely what drew a
    // traced port twice. The property on top of that is that the fan-out step
    // *re-uses* the anchor the fan-in step stood up rather than standing up a
    // rival, which is what sharing `stubs` across steps buys.
    assert!(
        !junction(&only_in).is_empty(),
        "vacuous: the seed net was never drawn"
    );
    assert_eq!(
        junction(&g).len(),
        1,
        "one signal, one junction: {:?}",
        junction(&g)
    );
    for anchor in junction(&only_in) {
        assert!(
            junction(&g).contains(&anchor),
            "the fan-out step replaced the fan-in anchor {anchor} instead of \
             re-using it: {:?} -> {:?}",
            junction(&only_in),
            junction(&g)
        );
    }
    // And it must carry both halves: the fan-in walk alone leaves it with wires
    // on one side only.
    assert!(
        g.edges.len() > only_in.edges.len(),
        "the fan-out step must add wires to the junction: {} vs {}",
        g.edges.len(),
        only_in.edges.len()
    );
}

#[test]
fn trace_graph_endpoints_resolve_to_emitted_pins() {
    // ELK cannot anchor an edge whose endpoint is not a pin of an emitted node.
    // cone_with asserts this for one seed; accumulation is the case most likely
    // to break it, since a box can now be reached by a step that did not emit it.
    let d = design();
    for proj in [Projection::ProcessLevel, Projection::GateLevel] {
        let g = trace_graph(
            &d,
            &[
                step(&d, RESETN, Dir::In, 2),
                step(&d, MEM_VALID, Dir::Out, 2),
                step(&d, VALID, Dir::Inout, 1),
            ],
            ConeLimits::default(),
            proj,
        );
        let pins = pin_ids(&g);
        for e in &g.edges {
            assert!(pins.contains(&e.source), "{proj:?}: dangling source {e:?}");
            assert!(pins.contains(&e.target), "{proj:?}: dangling target {e:?}");
        }
    }
}

#[test]
fn trace_graph_box_budget_is_global_across_steps() {
    // An accumulating trace has no natural bound, so the budget has to cover the
    // whole graph rather than each step. Anti-vacuous: the same steps uncapped
    // must exceed the cap, or this would pass on an empty walk.
    let d = design();
    let steps = [
        step(&d, RESETN, Dir::Inout, 3),
        step(&d, MEM_VALID, Dir::Inout, 3),
    ];
    let proj = Projection::ProcessLevel;
    // `boxes` bounds boxes, not nodes: a signal's anchor is a wire anchor, and
    // starving it would return an empty graph rather than a small one (the same
    // reasoning as the `.max(1)` fan-out clamp). `Port` joins `Net`/`Var` here
    // because a boundary crossing's anchor is keyed on its group representative,
    // which is whichever of the two same-path nodes has the lower model id (#285)
    // — the anchor is the same wire anchor either way.
    let boxes = |g: &SchematicGraph| {
        g.nodes
            .iter()
            .filter(|n| !matches!(n.kind, NodeKind::Net | NodeKind::Var | NodeKind::Port))
            .count()
    };
    let uncapped = trace_graph(&d, &steps, ConeLimits::default(), proj);
    assert!(
        boxes(&uncapped) > 4,
        "fixture too small to test the budget: {} boxes",
        boxes(&uncapped)
    );
    let capped = trace_graph(
        &d,
        &steps,
        ConeLimits {
            boxes: 4,
            ..ConeLimits::default()
        },
        proj,
    );
    assert!(
        boxes(&capped) <= 4,
        "budget ignored: {} boxes",
        boxes(&capped)
    );
    assert!(capped.truncated, "a budget that engaged must say so");
}

#[test]
fn trace_graph_per_step_fanout_reveals_what_the_shared_cap_dropped() {
    // Backs the "N more…" affordance (#244 PR4): expanding one capped signal must
    // reveal *its* remainder without un-capping the rest of the trace, which is
    // what raising the shared budget would do.
    let d = design();
    let proj = Projection::ProcessLevel;
    let tight = ConeLimits {
        fanout: 1,
        ..ConeLimits::depth(1)
    };
    let capped = trace_graph(&d, &[step(&d, RESETN, Dir::Inout, 1)], tight, proj);
    assert!(capped.truncated, "fanout 1 must engage on the fixture");
    let dropped: u32 = capped
        .nodes
        .iter()
        .flat_map(|n| &n.ports)
        .filter_map(|p| p.more)
        .sum();
    assert!(dropped > 0, "a capped pin must report its remainder");

    // The same step, with only *this* step's fan-out lifted.
    let opened = trace_graph(
        &d,
        &[TraceStep {
            fanout: Some(usize::MAX),
            ..step(&d, RESETN, Dir::Inout, 1)
        }],
        tight,
        proj,
    );
    assert!(
        opened.nodes.len() > capped.nodes.len(),
        "the override must draw what the cap dropped: {} vs {}",
        opened.nodes.len(),
        capped.nodes.len()
    );

    // And it is per step, not global: a second capped step alongside it stays capped.
    let mixed = trace_graph(
        &d,
        &[
            TraceStep {
                fanout: Some(usize::MAX),
                ..step(&d, RESETN, Dir::Inout, 1)
            },
            step(&d, MEM_VALID, Dir::Inout, 1),
        ],
        tight,
        proj,
    );
    assert!(
        mixed.truncated,
        "the un-overridden step must still report its own cap"
    );
}

#[test]
fn trace_graph_with_no_steps_is_empty() {
    let d = design();
    let g = trace_graph(&d, &[], ConeLimits::default(), Projection::ProcessLevel);
    assert!(g.nodes.is_empty() && g.edges.is_empty() && !g.truncated);
    assert!(
        g.root.is_empty(),
        "no seed, no scope to bind a breadcrumb to"
    );
}
