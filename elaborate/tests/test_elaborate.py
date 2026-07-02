"""Tests for the elaboration harness against the tier-1 fixture."""
from __future__ import annotations

from pathlib import Path

import pytest

from svxprobe_elaborate.elaborate import build_model
from svxprobe_elaborate.validate import _check_invariants, validate_model

REPO = Path(__file__).resolve().parents[2]
RTL = REPO / "fixtures" / "picorv32_soc" / "rtl"
SOURCES = [
    str(RTL / "picorv32.v"),
    str(RTL / "soc_pkg.sv"),
    str(RTL / "mem_if.sv"),
    str(RTL / "soc_mem.sv"),
    str(RTL / "picorv32_soc.sv"),
]


@pytest.fixture(scope="module")
def model() -> dict:
    return build_model(SOURCES, top="picorv32_soc")


def test_design_top(model: dict) -> None:
    assert model["design"] == "picorv32_soc"
    assert model["schema_version"] == 1


def test_schema_valid(model: dict) -> None:
    validate_model(model)
    assert _check_invariants(model) == []


def test_exercises_four_constructs(model: dict) -> None:
    """The fixture exists to stress generate / param / package / interface."""
    paths = {n["path"] for n in model["nodes"]}
    kinds = {n["kind"] for n in model["nodes"]}

    # generate-array expansion (the matcher's hardest case)
    assert "picorv32_soc.g_lane[0]" in paths
    assert "picorv32_soc.g_lane[1]" in paths
    assert "GenBlock" in kinds

    # parameterized instances: two core lanes
    assert "picorv32_soc.g_lane[0].core" in paths
    assert "picorv32_soc.g_lane[1].core" in paths

    # interface instance per lane, modelled as a distinct Interface bundle with
    # its modports (not an indistinguishable module Instance)
    assert "picorv32_soc.g_lane[0].bus" in paths
    assert "Interface" in kinds
    assert "Modport" in kinds

    # package-typed signal
    assert "picorv32_soc.g_lane[0].lane_state" in paths


def test_interface_instance_is_distinct_kind(model: dict) -> None:
    """An interface instance is its own `Interface` kind, not an indistinguishable
    `Instance`, so the schematic can give a signal bundle a distinct shape."""
    by = {n["path"]: n for n in model["nodes"]}
    bus = by["picorv32_soc.g_lane[0].bus"]
    assert bus["kind"] == "Interface", bus["kind"]
    # A plain module instance stays an Instance.
    assert by["picorv32_soc.g_lane[0].core"]["kind"] == "Instance"


def test_modports_emitted(model: dict) -> None:
    """The interface's modports emit as `Modport` children (views of the bundle)."""
    by = {n["path"]: n for n in model["nodes"]}
    core_mp = by.get("picorv32_soc.g_lane[0].bus.core")
    mem_mp = by.get("picorv32_soc.g_lane[0].bus.mem")
    assert core_mp and core_mp["kind"] == "Modport", core_mp
    assert mem_mp and mem_mp["kind"] == "Modport", mem_mp


def test_modport_nodes_carry_membership(model: dict) -> None:
    """Each Modport node carries its member signals + per-member direction as
    model data (#95): the two views of mem_if expose the same members with
    inverse driving directions. The modport stays a view — no children."""
    by = {n["path"]: n for n in model["nodes"]}
    core_mp = by["picorv32_soc.g_lane[0].bus.core"]
    mem_mp = by["picorv32_soc.g_lane[0].bus.mem"]

    def dirs(node: dict) -> dict[str, str]:
        return {m["name"]: m["dir"] for m in node["members"]}

    assert dirs(mem_mp) == {
        "clk": "in",
        "valid": "in",
        "instr": "in",
        "addr": "in",
        "wdata": "in",
        "wstrb": "in",
        "ready": "out",
        "rdata": "out",
    }, dirs(mem_mp)
    core = dirs(core_mp)
    assert core["valid"] == "out" and core["ready"] == "in", core
    assert set(core) == set(dirs(mem_mp)), "both views expose the same members"
    assert core_mp["children"] == [], "modport stays a view (no children)"
    # Membership is modport metadata only: other kinds don't carry the key.
    bus = by["picorv32_soc.g_lane[0].bus"]
    assert "members" not in bus


def test_consuming_interface_port_records_modport(model: dict) -> None:
    """A modport-specialized interface port (`mem_if.mem bus`) is an `Interface`
    node recording the view it selects, so the connection resolves to its own node
    rather than the consumer's box."""
    by = {n["path"]: n for n in model["nodes"]}
    port = by.get("picorv32_soc.g_lane[0].memory.bus")
    assert port and port["kind"] == "Interface", port
    assert port["modport"] == "mem", port.get("modport")


def test_modport_interface_port_pins(model: dict) -> None:
    """A modport-specialized interface port emits directional member pins (#64):
    one `Port` child per ModportPort, direction from the view, path pointing at
    the underlying bundle member, and an edge wiring pin -> member."""
    byid = {n["id"]: n for n in model["nodes"]}
    iface = next(
        n for n in model["nodes"] if n["path"] == "picorv32_soc.g_lane[0].memory.bus"
    )
    pins = {byid[c]["name"]: byid[c] for c in iface["children"]}
    # The full `mem` view, with slang's per-member directions.
    dirs = {name: p["dir"] for name, p in pins.items()}
    assert dirs == {
        "clk": "in",
        "valid": "in",
        "instr": "in",
        "addr": "in",
        "wdata": "in",
        "wstrb": "in",
        "ready": "out",
        "rdata": "out",
    }, dirs
    # Each pin is a view of the underlying member: same canonical path, and an
    # edge (same shape as a module port connection) wiring the two.
    assert pins["valid"]["path"] == "picorv32_soc.g_lane[0].bus.valid"
    assert pins["valid"]["kind"] == "Port"
    edge_ends = {
        (e["port"], byid[e["endpoint"]]["path"], e["dir"]) for e in model["edges"]
    }
    assert (pins["valid"]["id"], "picorv32_soc.g_lane[0].bus.valid", "in") in edge_ends
    assert (pins["ready"]["id"], "picorv32_soc.g_lane[0].bus.ready", "out") in edge_ends
    # The bare interface *instance* stays port-less: members carry both
    # directions, so there is nothing unambiguous to pin.
    bus = next(n for n in model["nodes"] if n["path"] == "picorv32_soc.g_lane[0].bus")
    assert not any(byid[c]["kind"] == "Port" and byid[c]["name"] == "valid"
                   for c in bus["children"])


def test_nodes_have_paths_and_keys(model: dict) -> None:
    for n in model["nodes"]:
        assert n["path"], f"node {n['id']} has empty path"
        assert n["symbol_key"], f"node {n['id']} has empty symbol_key"


def test_ports_carry_direction(model: dict) -> None:
    """Every Port node has a declared direction; non-ports leave it null."""
    ports = [n for n in model["nodes"] if n["kind"] == "Port"]
    assert ports, "no ports emitted"
    for n in ports:
        assert n["dir"] in ("in", "out", "inout"), f"port {n['path']} dir={n['dir']}"
    # A known input and output on the core.
    by = {(n["path"]): n for n in model["nodes"] if n["kind"] == "Port"}
    assert by["picorv32_soc.g_lane[0].core.clk"]["dir"] == "in"
    assert by["picorv32_soc.g_lane[0].core.eoi"]["dir"] == "out"


def test_constant_tied_inputs(model: dict) -> None:
    """Inputs tied to a literal carry that constant on the Port node."""
    by = {n["path"]: n for n in model["nodes"] if n["kind"] == "Port"}
    assert by["picorv32_soc.g_lane[0].core.irq"]["const"] == "32'd0"
    assert by["picorv32_soc.g_lane[0].core.pcpi_wr"]["const"] == "1'b0"
    # A net-driven input has no constant.
    assert by["picorv32_soc.g_lane[0].core.clk"]["const"] is None


def test_inferred_ff(model: dict) -> None:
    """An always_ff becomes an FF node wired to its clock and assigned output."""
    by = {n["id"]: n for n in model["nodes"]}
    ffs = [n for n in model["nodes"] if n["kind"] == "FF"]
    assert ffs, "no FF nodes emitted"

    def sigs(ff_id: int, direction: str) -> set[str]:
        return {
            by[e["endpoint"]]["name"]
            for e in model["edges"]
            if e["port"] == ff_id and e["dir"] == direction
        }

    lane = [f for f in ffs if "lane_state" in sigs(f["id"], "out")]
    assert lane, "no lane_state register"
    assert "clk" in sigs(lane[0]["id"], "in"), "FF not clocked"


def test_inferred_comb(model: dict) -> None:
    """Combinational logic (`always @*` / `always_comb` / continuous `assign`)
    becomes Comb nodes wired to the signals it reads (in) and assigns (out)."""
    by = {n["id"]: n for n in model["nodes"]}
    combs = [n for n in model["nodes"] if n["kind"] == "Comb"]
    assert combs, "no Comb nodes emitted"

    def sigs(cid: int, direction: str) -> set[str]:
        return {
            by[e["endpoint"]]["name"]
            for e in model["edges"]
            if e["port"] == cid and e["dir"] == direction
        }

    # At least one comb block is wired on both sides (reads something, drives
    # something) — real RTL has no free-floating combinational logic.
    assert any(
        sigs(c["id"], "in") and sigs(c["id"], "out") for c in combs
    ), "no comb box with both inputs and an output"


def test_continuous_assign_is_distinct_kind(model: dict) -> None:
    """A continuous `assign` is an `Assign` node (kept distinct from `Comb` so the
    schematic can draw it as a function node), wired to its reads and its LHS."""
    by = {n["id"]: n for n in model["nodes"]}
    assigns = [n for n in model["nodes"] if n["kind"] == "Assign"]
    combs = [n for n in model["nodes"] if n["kind"] == "Comb"]
    assert assigns, "no Assign nodes emitted"
    assert combs, "no Comb nodes emitted (always @* should not fold into Assign)"

    def sigs(nid: int, direction: str) -> set[str]:
        return {
            by[e["endpoint"]]["name"]
            for e in model["edges"]
            if e["port"] == nid and e["dir"] == direction
        }

    # A real continuous assign reads at least one signal and drives an output.
    assert any(
        sigs(a["id"], "in") and sigs(a["id"], "out") for a in assigns
    ), "no assign node with both inputs and an output"


def test_always_latch_is_distinct_kind(tmp_path) -> None:
    """An `always_latch` is its own `Latch` kind (not folded into `Comb`), wired to
    its data/enable in and its output. picorv32 has no latch, so use a snippet."""
    src = tmp_path / "latch_dut.sv"
    src.write_text(
        "module latch_dut(input en, input d, output logic q);\n"
        "  always_latch if (en) q = d;\n"
        "endmodule\n"
    )
    m = build_model([str(src)], top="latch_dut")
    by = {n["id"]: n for n in m["nodes"]}
    latches = [n for n in m["nodes"] if n["kind"] == "Latch"]
    assert latches, f"no Latch node; kinds={sorted({n['kind'] for n in m['nodes']})}"

    lid = latches[0]["id"]
    ins = {by[e["endpoint"]]["name"] for e in m["edges"] if e["port"] == lid and e["dir"] == "in"}
    outs = {by[e["endpoint"]]["name"] for e in m["edges"] if e["port"] == lid and e["dir"] == "out"}
    assert "q" in outs, outs
    assert ins & {"en", "d"}, ins


def _kinds(src: str, tmp_path) -> set[str]:
    f = tmp_path / "dut.sv"
    f.write_text(src)
    return {n["kind"] for n in build_model([str(f)], top="m")["nodes"]}


def test_always_comb_inferring_a_latch_is_latch(tmp_path) -> None:
    """An `always_comb` that accidentally infers a latch (incomplete assignment)
    is reclassified `Latch` from slang's own analysis-pass `InferredLatch`
    diagnostic — not a heuristic."""
    kinds = _kinds("module m(input en, d, output logic q);\n  always_comb if (en) q = d;\nendmodule\n", tmp_path)
    assert "Latch" in kinds and "Comb" not in kinds, kinds


def test_complete_always_comb_stays_comb(tmp_path) -> None:
    """A fully-assigned `always_comb` is combinational — not flagged, stays `Comb`."""
    kinds = _kinds(
        "module m(input en, d, output logic q);\n  always_comb if (en) q = d; else q = 0;\nendmodule\n",
        tmp_path,
    )
    assert "Comb" in kinds and "Latch" not in kinds, kinds


def test_legacy_level_always_is_not_reclassified(tmp_path) -> None:
    """slang does not flag a legacy level-sensitive `always` as inferring a latch
    (it is legal Verilog), so it stays `Comb` — we do not second-guess the model."""
    kinds = _kinds("module m(input en, d, output logic q);\n  always @* if (en) q = d;\nendmodule\n", tmp_path)
    assert "Comb" in kinds and "Latch" not in kinds, kinds


def test_legacy_clocked_always_is_ff(model: dict) -> None:
    """A legacy `always @(posedge clk)` inside a leaf core yields FF nodes — not
    only the top-level `always_ff` lane registers."""
    core_ffs = [
        n
        for n in model["nodes"]
        if n["kind"] == "FF" and "g_lane[0].core.$ff" in n["path"]
    ]
    assert core_ffs, "no FF nodes inside the core (legacy clocked always unmodeled)"


def test_connectivity_edges(model: dict) -> None:
    """Port connections are emitted as edges with valid endpoints."""
    edges = model["edges"]
    assert edges, "no edges emitted"
    byid = {n["id"]: n for n in model["nodes"]}
    ids = set(byid)
    for e in edges:
        assert e["port"] in ids and e["endpoint"] in ids, "edge endpoint out of range"
        assert e["dir"] in ("in", "out", "inout")

    def has_edge(port_path: str, endpoint_path: str) -> bool:
        return any(
            byid[e["port"]]["path"] == port_path
            and byid[e["endpoint"]]["path"] == endpoint_path
            for e in edges
        )

    # A core input pin wired to the top clock, and an output to the bus.
    assert has_edge("picorv32_soc.g_lane[0].core.clk", "picorv32_soc.clk")
    assert has_edge(
        "picorv32_soc.g_lane[0].core.mem_valid", "picorv32_soc.g_lane[0].bus.valid"
    )
    # The memory's modport-specialized interface port resolves to its own node
    # (not the consumer's box) and wires to the interface instance.
    assert has_edge("picorv32_soc.g_lane[0].memory.bus", "picorv32_soc.g_lane[0].bus")


def test_bus_bitselect_on_edges(model: dict) -> None:
    """A vector connected per-bit carries the resolved bit-select on its edge.

    `core_trap` is `logic[1:0]`; lane `gi` reads/drives `core_trap[gi]` both in
    its always_ff (FF edge) and via `.trap(core_trap[gi])` (port edge). Each such
    edge must record the *resolved* constant bit, e.g. `[0]` / `[1]`.
    """
    by = {n["id"]: n for n in model["nodes"]}
    ct = next(
        n["id"]
        for n in model["nodes"]
        if n["path"] == "picorv32_soc.core_trap" and n["kind"] == "Var"
    )
    # select on each edge landing on core_trap, keyed by the lane of the other end.
    lane_selects: dict[str, set] = {"g_lane[0]": set(), "g_lane[1]": set()}
    for e in model["edges"]:
        if e["endpoint"] != ct:
            continue
        other = by[e["port"]]["path"]
        for lane in lane_selects:
            if f".{lane}." in other:
                lane_selects[lane].add(e.get("select"))
    assert lane_selects["g_lane[0]"] == {"[0]"}, lane_selects
    assert lane_selects["g_lane[1]"] == {"[1]"}, lane_selects

    # A scalar, non-indexed connection carries no select.
    for e in model["edges"]:
        if (
            by[e["port"]]["path"] == "picorv32_soc.g_lane[0].core.clk"
            and by[e["endpoint"]]["path"] == "picorv32_soc.clk"
        ):
            assert e.get("select") is None


def test_enum_table_emitted(model: dict) -> None:
    """The fixture's packed enum `lane_state_e` is emitted in the normalized `enums`
    table, keyed by the same type string carried on its signals (#81)."""
    enums = model["enums"]
    assert "soc_pkg::lane_state_e" in enums
    e = enums["soc_pkg::lane_state_e"]
    assert e["width"] == 2
    assert e["members"] == [
        {"name": "LANE_RESET", "value": 0},
        {"name": "LANE_RUN", "value": 1},
        {"name": "LANE_TRAP", "value": 2},
    ]
    # The enum-typed signals reference the table via their `type`.
    lane = next(n for n in model["nodes"] if n["name"] == "lane_state" and n["kind"] == "Var")
    assert lane["type"] == "soc_pkg::lane_state_e"


def test_enum_values_explicit_and_positional(tmp_path) -> None:
    """Member values come from explicit SV literals, else the positional default
    (first = 0, otherwise prev + 1)."""
    src = tmp_path / "e.sv"
    src.write_text(
        "package q;\n"
        "  typedef enum logic [3:0] { S_IDLE, S_GO = 4'h4, S_DONE, S_ERR = 4'hF } st_e;\n"
        "endpackage\n"
        "module m; import q::*; st_e s; endmodule\n"
    )
    enums = build_model([str(src)], top="m")["enums"]
    assert enums["q::st_e"]["members"] == [
        {"name": "S_IDLE", "value": 0},
        {"name": "S_GO", "value": 4},
        {"name": "S_DONE", "value": 5},
        {"name": "S_ERR", "value": 15},
    ]


def test_enum_with_nonliteral_member_is_skipped(tmp_path) -> None:
    """An enum whose member uses a computed initializer is omitted entirely rather
    than guessed (no fabricated values)."""
    src = tmp_path / "bad.sv"
    src.write_text(
        "package q;\n"
        "  localparam int K = 1;\n"
        "  typedef enum logic [1:0] { A = K, B } bad_e;\n"
        "endpackage\n"
        "module m; import q::*; bad_e s; endmodule\n"
    )
    assert "q::bad_e" not in build_model([str(src)], top="m")["enums"]
