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

    # interface instance per lane
    assert "picorv32_soc.g_lane[0].bus" in paths

    # package-typed signal
    assert "picorv32_soc.g_lane[0].lane_state" in paths


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
    # The memory's interface port anchors to its box, wired to the bus instance.
    assert has_edge("picorv32_soc.g_lane[0].memory", "picorv32_soc.g_lane[0].bus")


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
