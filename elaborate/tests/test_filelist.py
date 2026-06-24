"""Tests for the EDA-style filelist parser."""
from __future__ import annotations

from pathlib import Path

from svxprobe_elaborate.elaborate import parse_filelist


def test_parses_sources_incdirs_and_nesting(tmp_path: Path) -> None:
    (tmp_path / "rtl").mkdir()
    (tmp_path / "inc").mkdir()
    (tmp_path / "rtl" / "a.sv").write_text("module a; endmodule\n")
    (tmp_path / "rtl" / "b.sv").write_text("module b; endmodule\n")

    nested = tmp_path / "nested.f"
    nested.write_text("rtl/b.sv\n+incdir+inc\n")

    main = tmp_path / "main.f"
    main.write_text(
        "// a comment\n"
        "rtl/a.sv        // inline comment\n"
        "-I inc\n"
        "-f nested.f\n"
        "+define+FOO=1\n"  # ignored directive
    )

    files: list[str] = []
    incdirs: list[str] = []
    parse_filelist(str(main), files, incdirs)

    assert [Path(f).name for f in files] == ["a.sv", "b.sv"]
    # both -I inc and +incdir+inc resolve to the same dir
    assert all(Path(d).name == "inc" for d in incdirs)
    assert len(incdirs) == 2
    # paths are absolute / resolved relative to each filelist's own dir
    assert all(Path(f).is_absolute() for f in files)


def test_handles_cycles(tmp_path: Path) -> None:
    a = tmp_path / "a.f"
    b = tmp_path / "b.f"
    a.write_text("-f b.f\n")
    b.write_text("-f a.f\n")  # cycle
    files: list[str] = []
    incdirs: list[str] = []
    parse_filelist(str(a), files, incdirs)  # must terminate
    assert files == []
