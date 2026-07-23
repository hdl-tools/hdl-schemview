"""Tests for explicitly declared C/C++ sources (#222).

``--hls-src`` lets a design declare where its C/C++ sources live instead of
relying on the provenance comment's path being resolvable relative to the
generated RTL's own directory. A declared *file* is registered even when no
comment references it (so an unmapped header is still browsable); a declared
*directory* is a search root used when resolving a comment's C path.

These exercise the resolution ladder in ``Elaborator._resolve_c_ref`` and the
eager registration in ``_register_declared_c_sources``. Declaring nothing must
reproduce the pre-#222 behavior exactly.
"""
from __future__ import annotations

from pathlib import Path
from typing import Any, Optional

from svxprobe_elaborate.elaborate import build_model

C_BODY = "int add(int a, int b) {\n  int s = a + b;\n  return s;\n}\n"


def _rtl(path: Path, c_ref: str) -> Path:
    """Generated-style RTL whose assign line cites `c_ref` line 2."""
    path.write_text(
        "module add(\n"
        "  input  [31:0] a,\n"
        "  input  [31:0] b,\n"
        "  output [31:0] s\n"
        ");\n"
        f"  assign s = a + b;  // {c_ref}:2\n"
        "endmodule\n"
    )
    return path


def _entry(model: dict[str, Any], suffix: str) -> Optional[dict[str, Any]]:
    """The files-table entry whose path ends with `suffix`, if registered."""
    return next((f for f in model["files"] if f["path"].endswith(suffix)), None)


def _c_ids(model: dict[str, Any]) -> set[int]:
    """File ids appearing as the C side of a source_map entry."""
    return {m["source"]["file"] for m in model.get("source_map", [])}


def test_declared_file_registered_even_with_no_provenance_comment(tmp_path: Path):
    """An unreferenced header is invisible without #222 — declaring it registers it."""
    (tmp_path / "add.cpp").write_text(C_BODY)
    (tmp_path / "helper.h").write_text("#pragma once\nint helper();\n")
    rtl = _rtl(tmp_path / "add.sv", "add.cpp")

    model = build_model(
        [str(rtl)], "add", hls_map=True, hls_src=[str(tmp_path / "helper.h")]
    )

    header = _entry(model, "helper.h")
    assert header is not None, "declared header should be registered"
    assert header["language"] == "c"
    # It has no RTL correspondence — registered purely so it can be browsed.
    assert header["id"] not in _c_ids(model)


def test_declared_file_resolves_a_c_source_that_is_not_beside_the_rtl(tmp_path: Path):
    """The pre-#222 resolver only looked next to the RTL; a sibling src/ tree missed."""
    src = tmp_path / "src"
    src.mkdir()
    (src / "add.cpp").write_text(C_BODY)
    rtl_dir = tmp_path / "rtl"
    rtl_dir.mkdir()
    rtl = _rtl(rtl_dir / "add.sv", "add.cpp")  # bare basename, C lives elsewhere

    # Without a declaration the C file cannot be found, so no mapping is emitted.
    bare = build_model([str(rtl)], "add", hls_map=True)
    assert bare.get("source_map", []) == []

    model = build_model([str(rtl)], "add", hls_map=True, hls_src=[str(src / "add.cpp")])
    assert len(model.get("source_map", [])) == 1
    cpp = _entry(model, "add.cpp")
    assert cpp is not None and cpp["language"] == "cpp"
    assert model.get("source_map", [])[0]["source"]["file"] == cpp["id"]


def test_declared_directory_acts_as_a_search_root(tmp_path: Path):
    """A directory resolves the comment's own relative path, and is not auto-scanned."""
    src = tmp_path / "src"
    src.mkdir()
    (src / "add.cpp").write_text(C_BODY)
    (src / "unrelated.cpp").write_text("int other() { return 0; }\n")
    rtl_dir = tmp_path / "rtl"
    rtl_dir.mkdir()
    rtl = _rtl(rtl_dir / "add.sv", "add.cpp")

    model = build_model([str(rtl)], "add", hls_map=True, hls_src=[str(src)])

    assert len(model.get("source_map", [])) == 1
    assert _entry(model, "add.cpp") is not None
    # A search root is not a bulk import: the sibling file stays out of the table.
    assert _entry(model, "unrelated.cpp") is None


def test_ambiguous_basename_is_not_resolved_by_guessing(tmp_path: Path):
    """Two declared sources sharing a basename must not be silently disambiguated."""
    a_dir, b_dir = tmp_path / "a", tmp_path / "b"
    a_dir.mkdir()
    b_dir.mkdir()
    (a_dir / "add.cpp").write_text(C_BODY)
    (b_dir / "add.cpp").write_text(C_BODY)
    rtl_dir = tmp_path / "rtl"
    rtl_dir.mkdir()
    rtl = _rtl(rtl_dir / "add.sv", "add.cpp")

    model = build_model(
        [str(rtl)],
        "add",
        hls_map=True,
        hls_src=[str(a_dir / "add.cpp"), str(b_dir / "add.cpp")],
    )

    # Neither is picked for the mapping; both are still registered as browsable.
    assert model.get("source_map", []) == []
    assert len([f for f in model["files"] if f["path"].endswith("add.cpp")]) == 2


def test_unresolvable_absolute_ref_falls_through_to_a_declared_source(tmp_path: Path):
    """A vendor-baked absolute path from the build machine still resolves locally."""
    (tmp_path / "add.cpp").write_text(C_BODY)
    rtl = _rtl(tmp_path / "add.sv", "/build/agent/workspace/add.cpp")

    bare = build_model([str(rtl)], "add", hls_map=True)
    assert bare.get("source_map", []) == [], "absolute path off the build machine cannot resolve"

    model = build_model(
        [str(rtl)], "add", hls_map=True, hls_src=[str(tmp_path / "add.cpp")]
    )
    assert len(model.get("source_map", [])) == 1


def test_declaring_nothing_reproduces_pre_222_behavior(tmp_path: Path):
    """The common case (C beside the RTL) is unchanged, with and without the flag."""
    (tmp_path / "add.cpp").write_text(C_BODY)
    rtl = _rtl(tmp_path / "add.sv", "add.cpp")

    without = build_model([str(rtl)], "add", hls_map=True)
    with_empty = build_model([str(rtl)], "add", hls_map=True, hls_src=[])

    assert without == with_empty
    assert len(without.get("source_map", [])) == 1


def test_hls_src_is_inert_without_hls_map(tmp_path: Path):
    """Declared sources are part of the opt-in pass; off by default stays byte-identical."""
    (tmp_path / "add.cpp").write_text(C_BODY)
    (tmp_path / "helper.h").write_text("#pragma once\n")
    rtl = _rtl(tmp_path / "add.sv", "add.cpp")

    plain = build_model([str(rtl)], "add")
    declared = build_model([str(rtl)], "add", hls_src=[str(tmp_path / "helper.h")])

    assert plain == declared
    assert all("language" not in f for f in plain["files"])
