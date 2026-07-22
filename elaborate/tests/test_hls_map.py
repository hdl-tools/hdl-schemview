"""Tests for the opt-in HLS C/C++ ↔ RTL provenance pass (#159).

The harness gains an opt-in ``--hls-map`` mode that scans the generated RTL's
line-annotated provenance comments (e.g. ``// add.cpp:3``) for the originating
C source and emits a ``source_map`` linking generated-RTL spans to C/C++ spans.
With the flag off the output is byte-identical to the default; these tests
exercise the flag *on* against a tiny synthetic HLS-style fixture.
"""
from __future__ import annotations

from pathlib import Path

from svxprobe_elaborate.elaborate import build_model


def _hls_design(tmp_path: Path) -> Path:
    """Write a tiny C source + generated-style RTL carrying provenance comments.

    Returns the RTL path. The RTL's `assign` lines each name the C line they
    came from, mimicking what an HLS tool embeds.
    """
    (tmp_path / "add.cpp").write_text(
        "int add(int a, int b) {\n"  # line 1
        "  int s = a + b;\n"  # line 2
        "  return s;\n"  # line 3
        "}\n"  # line 4
    )
    rtl = tmp_path / "add.sv"
    rtl.write_text(
        "module add(\n"  # line 1
        "  input  [31:0] a,\n"  # line 2
        "  input  [31:0] b,\n"  # line 3
        "  output [31:0] s\n"  # line 4
        ");\n"  # line 5
        "  assign s = a + b;  // add.cpp:2\n"  # line 6 → C line 2
        "endmodule\n"  # line 7
    )
    return rtl


def test_hls_map_emits_source_map(tmp_path: Path) -> None:
    rtl = _hls_design(tmp_path)
    model = build_model([str(rtl)], top="add", hls_map=True)

    # A correspondence was emitted, linking the RTL assign line to the C line.
    assert model.get("source_map"), "source_map present with --hls-map"
    entry = model["source_map"][0]
    assert entry["generated"]["start"]["line"] == 6  # the assign line
    assert entry["source"]["start"]["line"] == 2  # C `int s = a + b;`

    # The C file is registered and tagged; the RTL file is tagged systemverilog.
    files = {f["path"].rsplit("/", 1)[-1]: f for f in model["files"]}
    assert files["add.cpp"]["language"] == "cpp"
    assert files["add.sv"]["language"] == "systemverilog"

    # The mapped ranges reference real file ids in the table.
    ids = {f["id"] for f in model["files"]}
    assert entry["generated"]["file"] in ids
    assert entry["source"]["file"] in ids
    assert entry["source"]["file"] == files["add.cpp"]["id"]


def test_hls_map_off_is_byte_identical(tmp_path: Path) -> None:
    """Flag off ⇒ no source_map key and no `language` on file entries."""
    rtl = _hls_design(tmp_path)
    model = build_model([str(rtl)], top="add", hls_map=False)
    assert "source_map" not in model
    assert all("language" not in f for f in model["files"])


def test_hls_comment_re_override(tmp_path: Path) -> None:
    """A vendor-specific comment form is matched via the regex override."""
    (tmp_path / "k.c").write_text("a;\nb;\nc;\n")
    rtl = tmp_path / "k.sv"
    rtl.write_text(
        "module k(output x);\n"
        "  assign x = 1'b0;  // src=k.c line=3\n"
        "endmodule\n"
    )
    model = build_model(
        [str(rtl)],
        top="k",
        hls_map=True,
        hls_comment_re=r"src=(?P<file>[\w./-]+) line=(?P<line>\d+)",
    )
    assert model.get("source_map"), "override pattern matched"
    assert model["source_map"][0]["source"]["start"]["line"] == 3
