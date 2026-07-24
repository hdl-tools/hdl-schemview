"""Tests for the optional identifier-occurrence pass (#225).

The harness gains an opt-in `--name-refs` mode that records every declaration name
token and every resolved value reference as a `(span, class, scope-relative path)`
record. With the flag off the output is byte-identical to the default; these tests
exercise the flag *on* against small inline modules.

The classification always comes from the symbol the elaboration resolved — an
identifier it cannot classify yields no record, so the source pane leaves it
default-colored rather than guessing.
"""
from __future__ import annotations

from pathlib import Path

from svxprobe_elaborate.elaborate import build_model
from svxprobe_elaborate.validate import _check_invariants, validate_model


def _model(tmp_path: Path, body: str, name_refs: bool = True, top: str = "m") -> dict:
    """Elaborate a one-module design wrapping `body` (the module's contents)."""
    src = tmp_path / "m.sv"
    src.write_text(f"module m(\n{body}\nendmodule\n")
    return build_model([str(src)], top=top, name_refs=name_refs)


def _text(tmp_path: Path) -> bytes:
    return (tmp_path / "m.sv").read_bytes()


def _spans(model: dict, tmp_path: Path) -> list[tuple[str, str, str]]:
    """(source text, class, rel) for every emitted ref."""
    data = _text(tmp_path)
    return [
        (data[r["offset"] : r["offset"] + r["len"]].decode(), r["class"], r["rel"])
        for r in model.get("name_refs", [])
    ]


def test_flag_off_emits_nothing(tmp_path: Path) -> None:
    """Default output carries no `name_refs` key at all — byte-identical to pre-#225."""
    body = "  input logic a, b, output logic y);\n  assign y = a & b;"
    off = _model(tmp_path, body, name_refs=False)
    assert "name_refs" not in off
    on = _model(tmp_path, body)
    assert on["name_refs"]
    # The pass is purely additive: nothing else about the model moves.
    assert {k: v for k, v in on.items() if k != "name_refs"} == off


def test_usage_span_resolves_to_the_signal(tmp_path: Path) -> None:
    """A reference inside a process is recorded with the signal's scope-relative path
    — the span the model previously had no way to attribute to anything but the
    enclosing block."""
    model = _model(
        tmp_path,
        "  input logic clk, d, output logic q);\n"
        "  always_ff @(posedge clk) q <= d;",
    )
    spans = _spans(model, tmp_path)
    # `d` is read inside the always_ff body, and resolves to the signal `d`.
    assert ("d", "signal", "d") in spans
    assert ("clk", "signal", "clk") in spans
    assert _check_invariants(model) == []
    validate_model(model)  # raises if the new records violate the schema


def test_declaration_name_tokens_are_classified(tmp_path: Path) -> None:
    """Declarations contribute their own name token, classed by what they declare."""
    model = _model(
        tmp_path,
        "  input logic a, output logic y);\n"
        "  parameter int W = 4;\n"
        "  logic [W-1:0] tmp;\n"
        "  assign y = a & tmp[0];",
    )
    spans = _spans(model, tmp_path)
    assert ("m", "module", "") in spans, spans  # `module m` names the scope itself
    assert ("W", "param", "W") in spans
    assert ("tmp", "signal", "tmp") in spans
    # A port declaration wins the class over its backing net on the shared token.
    assert ("a", "port", "a") in spans


def test_spans_are_exact_and_deduped(tmp_path: Path) -> None:
    """One record per span, each covering exactly its identifier."""
    model = _model(
        tmp_path,
        "  input logic a, b, output logic y);\n  assign y = a & b & a;",
    )
    refs = model["name_refs"]
    keys = [(r["file"], r["offset"]) for r in refs]
    assert len(keys) == len(set(keys))
    # Sorted by (file, offset) so the golden is stable.
    assert keys == sorted(keys)
    data = _text(tmp_path)
    starts = [0] + [i + 1 for i, ch in enumerate(data) if ch == 0x0A]
    for r in refs:
        token = data[r["offset"] : r["offset"] + r["len"]].decode()
        assert token.strip() == token and token
        # line/col agree with the offset (the frontend renders against line/col,
        # the resolver looks up by offset — they must describe the same place).
        line = max(i for i, s in enumerate(starts) if s <= r["offset"])
        assert r["line"] == line + 1
        assert r["col"] == r["offset"] - starts[line] + 1
    # `a` appears twice in the same expression — two distinct spans, both recorded.
    assert sum(1 for r in refs if data[r["offset"] : r["offset"] + r["len"]] == b"a") >= 2


def test_one_span_per_source_site_across_instances(tmp_path: Path) -> None:
    """A module instantiated twice yields **one** record per source span, whose `rel`
    is instance-invariant — that is what lets a single span serve every instance."""
    src = tmp_path / "dual.sv"
    src.write_text(
        "module leaf(input logic a, output logic y);\n"
        "  assign y = ~a;\n"
        "endmodule\n"
        "module top(input logic p, q, output logic r, s);\n"
        "  leaf u0(.a(p), .y(r));\n"
        "  leaf u1(.a(q), .y(s));\n"
        "endmodule\n"
    )
    model = build_model([str(src)], top="top", name_refs=True)
    data = src.read_bytes()
    refs = model["name_refs"]
    keys = [(r["file"], r["offset"]) for r in refs]
    assert len(keys) == len(set(keys)), "a span must not be recorded once per instance"
    # The reference to `a` inside leaf is stored relative to leaf, not to top.u0.
    inside = [
        r
        for r in refs
        if data[r["offset"] : r["offset"] + r["len"]] == b"a" and r["line"] == 2
    ]
    assert inside and all(r["rel"] == "a" for r in inside), inside
    # The instance names themselves are relative to the enclosing scope.
    assert any(r["rel"] == "u0" and r["class"] == "instance" for r in refs)


def test_out_of_scope_symbol_is_stored_absolute(tmp_path: Path) -> None:
    """A package parameter is not under any instance, so it is stored absolute with a
    `/` marker rather than being silently mis-scoped."""
    src = tmp_path / "pkg.sv"
    src.write_text(
        "package p;\n  parameter int K = 3;\nendpackage\n"
        "module m(output logic [7:0] y);\n"
        "  assign y = p::K;\n"
        "endmodule\n"
    )
    model = build_model([str(src)], top="m", name_refs=True)
    data = src.read_bytes()
    hits = [
        r
        for r in model["name_refs"]
        if data[r["offset"] : r["offset"] + r["len"]] == b"p::K"
    ]
    assert hits, model["name_refs"]
    assert all(r["rel"].startswith("/") and r["class"] == "param" for r in hits)
