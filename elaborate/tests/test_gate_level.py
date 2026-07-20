"""Tests for the optional gate-level projection extraction pass (#157, PR1).

The harness gains an opt-in `--gate-level` mode that decomposes process/assign
RHS expressions into gate/mux primitive nodes (ADR 0005). With the flag off the
output is byte-identical to the process-level default; these tests exercise the
flag *on* against small inline modules.
"""
from __future__ import annotations

from pathlib import Path

from svxprobe_elaborate.elaborate import build_model
from svxprobe_elaborate.validate import _check_invariants, validate_model

GATE_KINDS = {
    "And", "Or", "Xor", "Xnor", "Nand", "Nor", "Not", "Buf",
    "Add", "Sub", "Mul", "Cmp", "Shift", "Mux",
}


def _model(tmp_path: Path, body: str, gate_level: bool = True) -> dict:
    """Elaborate a one-module design wrapping `body` (the module's contents)."""
    src = tmp_path / "m.sv"
    src.write_text(f"module m(\n{body}\nendmodule\n")
    return build_model([str(src)], top="m", gate_level=gate_level)


def _by_id(model: dict) -> dict[int, dict]:
    return {n["id"]: n for n in model["nodes"]}


def _inputs(model: dict, gate_id: int) -> list[dict]:
    """Endpoint nodes wired into `gate_id` as inputs."""
    by = _by_id(model)
    return [by[e["endpoint"]] for e in model["edges"]
            if e["port"] == gate_id and e["dir"] == "in"]


def _outputs(model: dict, gate_id: int) -> list[dict]:
    by = _by_id(model)
    return [by[e["endpoint"]] for e in model["edges"]
            if e["port"] == gate_id and e["dir"] == "out"]


def _gates(model: dict, kind: str) -> list[dict]:
    return [n for n in model["nodes"] if n["kind"] == kind]


def test_and_chain_collapses_to_one_gate(tmp_path: Path) -> None:
    """`a & b & c` is one 3-input And gate, not two chained 2-input ANDs."""
    model = _model(
        tmp_path,
        "  input logic a, b, c, output logic y);\n"
        "  assign y = a & b & c;",
    )
    gates = _gates(model, "And")
    assert len(gates) == 1, [n["kind"] for n in model["nodes"]]
    g = gates[0]
    # Sub-expression source range → cross-probe stays a lookup.
    assert g["def_range"] is not None
    # Flat child of the logic (assign) node, with a synthetic path segment.
    assert "$and" in g["path"]
    assert {n["name"] for n in _inputs(model, g["id"])} == {"a", "b", "c"}
    assert {n["name"] for n in _outputs(model, g["id"])} == {"y"}
    # Referential integrity holds for the augmented model.
    assert _check_invariants(model) == []


def _mux_edge(model: dict, gate_id: int, port: str) -> dict | None:
    for e in model["edges"]:
        if e["port"] == gate_id and e.get("mux_port") == port:
            return e
    return None


def test_gate_def_range_is_a_subexpression(tmp_path: Path) -> None:
    """A primitive's def_range is its own sub-expression span (within one line),
    not the whole assign construct — the scoped ADR-0004 relaxation of ADR 0005."""
    model = _model(
        tmp_path,
        "  input logic a, b, output logic y);\n"
        "  assign y = a & b;",
    )
    g = _gates(model, "And")[0]
    r = g["def_range"]
    assert r is not None and r["start"]["line"] == r["end"]["line"]
    # `a & b` starts after the `assign y = ` prefix, so past column 1.
    assert r["start"]["col"] > 1


def test_ternary_becomes_mux(tmp_path: Path) -> None:
    """`sel ? p : q` is one Mux: select on its own port, true→D1, false→D0."""
    model = _model(
        tmp_path,
        "  input logic sel, p, q, output logic y);\n"
        "  assign y = sel ? p : q;",
    )
    muxes = _gates(model, "Mux")
    assert len(muxes) == 1
    m = muxes[0]
    by = _by_id(model)
    assert by[_mux_edge(model, m["id"], "sel")["endpoint"]]["name"] == "sel"
    assert by[_mux_edge(model, m["id"], "d1")["endpoint"]]["name"] == "p"
    assert by[_mux_edge(model, m["id"], "d0")["endpoint"]]["name"] == "q"
    assert {n["name"] for n in _outputs(model, m["id"])} == {"y"}


def _consts(model: dict) -> list[dict]:
    return [n for n in model["nodes"] if n["kind"] == "Const"]


def test_literal_operand_becomes_const_tie(tmp_path: Path) -> None:
    """A hard-coded literal operand (`a & 8'hFF`, #199) surfaces as a `Const` tie
    node carrying the value, wired into the gate alongside the real signal."""
    model = _model(
        tmp_path,
        "  input logic [7:0] a, output logic [7:0] y);\n"
        "  assign y = a & 8'hFF;",
    )
    g = _gates(model, "And")[0]
    ins = _inputs(model, g["id"])
    consts = [n for n in ins if n["kind"] == "Const"]
    assert len(consts) == 1, [(n["kind"], n["name"]) for n in ins]
    assert consts[0]["const"], "Const node carries the literal value"
    assert consts[0]["def_range"] is not None
    # The real signal operand is still wired.
    assert any(n["name"] == "a" for n in ins)
    assert _check_invariants(model) == []


def test_parameter_operand_carries_value(tmp_path: Path) -> None:
    """A parameter operand (`a & MASK`, #199) wires to its real `Param` node —
    preserving cross-probe — now stamped with its resolved value."""
    model = _model(
        tmp_path,
        "  input logic [7:0] a, output logic [7:0] y);\n"
        "  localparam logic [7:0] MASK = 8'hAA;\n"
        "  assign y = a & MASK;",
    )
    g = _gates(model, "And")[0]
    ins = _inputs(model, g["id"])
    params = [n for n in ins if n["kind"] == "Param"]
    assert len(params) == 1, [(n["kind"], n["name"]) for n in ins]
    assert params[0]["name"] == "MASK"
    assert params[0]["const"], "Param node carries its resolved value"
    assert any(n["name"] == "a" for n in ins)
    assert _check_invariants(model) == []


def test_x_default_mux_branch_ties_not_dropped(tmp_path: Path) -> None:
    """A `sel ? a : 'x` don't-care branch (#199) ties as a Const instead of
    vanishing, so the Mux keeps all three inputs. slang leaves `.constant` unset
    for an `'x` literal, so the value comes from `.value`."""
    model = _model(
        tmp_path,
        "  input logic sel, a, output logic y);\n"
        "  assign y = sel ? a : 1'bx;",
    )
    mux = _gates(model, "Mux")[0]
    ins = _inputs(model, mux["id"])
    assert len(ins) == 3, [(n["kind"], n["name"]) for n in ins]
    consts = [n for n in ins if n["kind"] == "Const"]
    assert len(consts) == 1 and consts[0]["const"], "x-branch tied as a Const"


def test_concat_operand_becomes_concat_box(tmp_path: Path) -> None:
    """A concatenation data branch (`{a[31:2], 2'b00}`, #199) becomes a `Concat`
    primitive gathering its elements, so the Mux keeps all three inputs (matches
    picorv32's `mem_la_addr`)."""
    model = _model(
        tmp_path,
        "  input logic [31:0] a, input logic sel, output logic [31:0] y);\n"
        "  assign y = sel ? {a[31:2], 2'b00} : a;",
    )
    mux = _gates(model, "Mux")[0]
    ins = _inputs(model, mux["id"])
    assert len(ins) == 3, [(n["kind"], n["name"]) for n in ins]
    assert any(n["kind"] == "Concat" for n in ins), "the concat branch is a Concat box"
    # The Concat gathers its elements: the signal slice and the 2'b00 const tie.
    concat = _gates(model, "Concat")[0]
    assert any(n["kind"] == "Const" for n in _inputs(model, concat["id"])), "concat const"


def test_const_operand_off_flag_stays_process_level(tmp_path: Path) -> None:
    """With the flag off, a literal operand emits no `Const` node (byte-identical)."""
    model = _model(
        tmp_path,
        "  input logic [7:0] a, output logic [7:0] y);\n"
        "  assign y = a & 8'hFF;",
        gate_level=False,
    )
    assert _consts(model) == []


def test_not_over_gate_folds_to_nand(tmp_path: Path) -> None:
    """`~(a & b)` folds the bubble onto the gate: one Nand, no separate And+Not."""
    model = _model(
        tmp_path,
        "  input logic a, b, output logic y);\n"
        "  assign y = ~(a & b);",
    )
    assert len(_gates(model, "Nand")) == 1
    assert _gates(model, "And") == []
    assert _gates(model, "Not") == []
    n = _gates(model, "Nand")[0]
    assert {x["name"] for x in _inputs(model, n["id"])} == {"a", "b"}


def test_not_over_signal_is_a_standalone_inverter(tmp_path: Path) -> None:
    """`~a` over a bare signal stays a standalone Not (inverter triangle+bubble)."""
    model = _model(
        tmp_path,
        "  input logic a, output logic y);\n"
        "  assign y = ~a;",
    )
    assert len(_gates(model, "Not")) == 1
    assert {x["name"] for x in _inputs(model, _gates(model, "Not")[0]["id"])} == {"a"}


def test_reduction_and_is_a_single_input_and(tmp_path: Path) -> None:
    """A reduction-AND (`&a`) is an And gate with one wide input."""
    model = _model(
        tmp_path,
        "  input logic [3:0] a, output logic y);\n"
        "  assign y = &a;",
    )
    ands = _gates(model, "And")
    assert len(ands) == 1
    assert {x["name"] for x in _inputs(model, ands[0]["id"])} == {"a"}


def test_negated_reductions_fold_to_bubbled_gates(tmp_path: Path) -> None:
    """`~&a` / `~|a` / `~^a` are single-input Nand/Nor/Xnor (base gate + bubble),
    per ADR 0005 §2 — not silently dropped."""
    model = _model(
        tmp_path,
        "  input logic [3:0] a, output logic n, o, x);\n"
        "  assign n = ~&a;\n"
        "  assign o = ~|a;\n"
        "  assign x = ~^a;",
    )
    assert len(_gates(model, "Nand")) == 1
    assert len(_gates(model, "Nor")) == 1
    assert len(_gates(model, "Xnor")) == 1
    # Each is a reduction over the one wide input `a`.
    assert {n["name"] for n in _inputs(model, _gates(model, "Nand")[0]["id"])} == {"a"}


def test_unary_plus_is_a_buffer(tmp_path: Path) -> None:
    """Unary `+` is identity → a Buf (bare triangle, ADR 0005 §2)."""
    model = _model(
        tmp_path,
        "  input logic [3:0] a, output logic [3:0] y);\n"
        "  assign y = +a;",
    )
    assert len(_gates(model, "Buf")) == 1
    assert {n["name"] for n in _inputs(model, _gates(model, "Buf")[0]["id"])} == {"a"}


def test_datapath_kinds_carry_operator_label(tmp_path: Path) -> None:
    """`+`/`-`/`*`/compare/shift map to distinct datapath kinds, each keeping the
    exact operator on `op` (the coarse kind can't tell `==` from `<`)."""
    model = _model(
        tmp_path,
        "  input logic [3:0] a, b, output logic [3:0] s, d, p, sh,"
        " output logic c);\n"
        "  assign s = a + b;\n"
        "  assign d = a - b;\n"
        "  assign p = a * b;\n"
        "  assign c = a < b;\n"
        "  assign sh = a << b;",
    )
    kinds = {n["kind"] for n in model["nodes"] if n["kind"] in GATE_KINDS}
    assert {"Add", "Sub", "Mul", "Cmp", "Shift"} <= kinds
    cmp_node = _gates(model, "Cmp")[0]
    assert cmp_node["op"] == "LessThan"
    assert _gates(model, "Shift")[0]["op"] == "LogicalShiftLeft"


def test_mux_composes_with_gates(tmp_path: Path) -> None:
    """`sel ? a & b : c & d` -> a Mux whose two data inputs are each an And."""
    model = _model(
        tmp_path,
        "  input logic sel, a, b, c, dd, output logic y);\n"
        "  assign y = sel ? a & b : c & dd;",
    )
    assert len(_gates(model, "Mux")) == 1
    assert len(_gates(model, "And")) == 2
    m = _gates(model, "Mux")[0]
    by = _by_id(model)
    # D1 and D0 endpoints are And nodes (gate-to-gate wiring), not raw signals.
    assert by[_mux_edge(model, m["id"], "d1")["endpoint"]]["kind"] == "And"
    assert by[_mux_edge(model, m["id"], "d0")["endpoint"]]["kind"] == "And"
    assert by[_mux_edge(model, m["id"], "sel")["endpoint"]]["name"] == "sel"
    assert _check_invariants(model) == []


def test_gates_are_children_of_the_logic_block(tmp_path: Path) -> None:
    """Primitives are flat children of their process/assign node, so a gate-level
    schematic can expand exactly that block into its network."""
    model = _model(
        tmp_path,
        "  input logic a, b, output logic y);\n"
        "  assign y = a & b;",
    )
    by = _by_id(model)
    g = _gates(model, "And")[0]
    parent = by[g["parent"]]
    assert parent["kind"] == "Assign"
    assert g["id"] in parent["children"]


# --- the byte-identical guarantee: flag OFF changes nothing --------------------

REPO = Path(__file__).resolve().parents[2]
RTL = REPO / "fixtures" / "picorv32_soc" / "rtl"
SOURCES = [
    str(RTL / "picorv32.v"),
    str(RTL / "soc_pkg.sv"),
    str(RTL / "mem_if.sv"),
    str(RTL / "soc_mem.sv"),
    str(RTL / "picorv32_soc.sv"),
]


def test_default_output_has_no_gate_primitives() -> None:
    """With the flag off the fixture carries no gate/mux kinds and no mux_port
    edges — the process-level default is untouched."""
    model = build_model(SOURCES, top="picorv32_soc")
    assert not (GATE_KINDS & {n["kind"] for n in model["nodes"]})
    assert all("mux_port" not in e for e in model["edges"])


def test_gate_level_output_validates_against_schema(tmp_path: Path) -> None:
    """The gate-level projection is part of the model contract (PR2): the new
    gate/mux `kind`s, the `op` node field, and `mux_port` edges all pass the JSON
    schema. (PR1 only asserted referential integrity; the schema lands here.)"""
    model = _model(
        tmp_path,
        "  input logic sel, a, b, output logic [3:0] c, dd, output logic y,"
        " output logic [3:0] z);\n"
        "  assign y = sel ? a & b : a;\n"
        "  assign z = c < dd ? c : dd;",
    )
    # Sanity: the flag actually produced primitives (an op-carrying Cmp + a Mux)
    # for the schema to validate — otherwise this would pass vacuously.
    kinds = {n["kind"] for n in model["nodes"]}
    assert {"Mux", "And", "Cmp"} <= kinds
    assert any(n.get("op") for n in model["nodes"])
    assert any("mux_port" in e for e in model["edges"])
    validate_model(model)  # raises jsonschema.ValidationError if the schema drifts


def test_gate_level_is_purely_additive_on_the_fixture() -> None:
    """Turning the flag on adds gate/mux nodes but preserves every process-level
    node and edge (it is an additional projection, not a replacement)."""
    base = build_model(SOURCES, top="picorv32_soc")
    gl = build_model(SOURCES, top="picorv32_soc", gate_level=True)

    base_paths = {n["path"] for n in base["nodes"]}
    gl_paths = {n["path"] for n in gl["nodes"]}
    # Every original node survives unchanged; only synthetic primitives are added.
    assert base_paths <= gl_paths
    added = [n for n in gl["nodes"] if n["path"] not in base_paths]
    assert added and all(n["kind"] in GATE_KINDS | {"Const", "Concat"} for n in added)
    assert all(n["def_range"] is not None for n in added)
    assert _check_invariants(gl) == []

    # Every process-level edge survives too (id is recomputed by the sort over the
    # enlarged set, so key on the endpoints/dir/selects, not id). Map each edge's
    # endpoints back to canonical paths so the comparison is projection-stable.
    def edge_keys(model: dict) -> set:
        by = _by_id(model)
        return {
            (by[e["port"]]["path"], by[e["endpoint"]]["path"], e["dir"],
             e.get("select"), e.get("mem_port"), e.get("mux_port"))
            for e in model["edges"]
        }

    assert edge_keys(base) <= edge_keys(gl)
