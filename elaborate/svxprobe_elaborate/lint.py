"""Lint a SystemVerilog design for illegal procedural-driver combinations.

Targets a rule that strict simulators (notably VCS) enforce but slang/pyslang
and Verilator silently accept: a variable written by an ``always_ff`` block must
have *no other* procedural driver (IEEE 1800 §9.2.2.4). The classic offender is a
memory array initialized with ``$readmemh`` in an ``initial`` block and also
written from ``always_ff`` -- it elaborates cleanly here yet fails VCS with
``Error-[ICPD] Illegal combination of drivers``.

slang models the output (memory) argument of ``$readmem*`` as an assignment, so
that init path is detected the same way as a plain procedural assignment.

Usable as a library (``lint_files`` / ``lint_compilation``) and as a CLI:
    python -m svxprobe_elaborate.lint --top picorv32_soc fixtures/.../*.sv
The CLI exits non-zero when any violation is found, so it can gate CI.
"""
from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from typing import Any, Iterator, Optional

from pyslang import Bag, SourceManager
from pyslang.ast import Compilation, CompilationOptions
from pyslang.syntax import SyntaxTree

from .elaborate import parse_filelist


def _kind_name(sym: Any) -> str:
    """Bare kind name, e.g. ``ProceduralBlock`` or ``NamedValue``."""
    return str(getattr(sym, "kind", "")).rsplit(".", 1)[-1]


def _proc_kind(block: Any) -> str:
    """Procedure kind of a procedural block, e.g. ``AlwaysFF`` / ``Initial``."""
    return str(getattr(block, "procedureKind", "")).rsplit(".", 1)[-1]


@dataclass
class Finding:
    """A symbol illegally driven by ``always_ff`` plus another process."""

    path: str
    others: list[str]  # the *other* procedure kinds, sorted (e.g. ['Initial'])
    location: Optional[str] = None  # "file:line" of the symbol declaration, if known

    def message(self) -> str:
        where = f" ({self.location})" if self.location else ""
        cause = (
            f"also written by {' + '.join(self.others)}"
            if self.others
            else "written by more than one always_ff block"
        )
        return (
            f"{self.path}{where}: written by always_ff and {cause} -- illegal "
            f"combination of procedural drivers (rejected by VCS as "
            f"Error-[ICPD]); use a plain `always` block for memories initialized "
            f"via $readmemh"
        )


def _members(sym: Any) -> Any:
    body = getattr(sym, "body", None)
    if body is not None:
        return body
    if getattr(sym, "isScope", False):
        return sym
    return None


def _walk_blocks(sym: Any) -> Iterator[Any]:
    """Yield every ProceduralBlock reachable through instances/generate scopes."""
    if _kind_name(sym) == "ProceduralBlock":
        yield sym
        return
    members = _members(sym)
    if members is None:
        return
    try:
        children = list(members)
    except TypeError:
        return
    for child in children:
        yield from _walk_blocks(child)


def _written_symbols(block: Any) -> dict[str, Any]:
    """Map hierarchical path -> value symbol for everything `block` assigns.

    Catches both ordinary procedural assignments and the memory argument of
    ``$readmem*`` system tasks, which slang represents as an assignment to that
    argument.
    """
    out: dict[str, Any] = {}

    def collect(node: Any) -> None:
        k = _kind_name(node)
        if "NamedValue" in k or "HierarchicalValue" in k:
            s = getattr(node, "symbol", None)
            p = getattr(s, "hierarchicalPath", None)
            if s is not None and p:
                out[p] = s

    def cb(node: Any) -> None:
        if "Assignment" in _kind_name(node):
            left = getattr(node, "left", None)
            if left is not None:
                try:
                    left.visit(collect)
                except Exception:
                    pass

    try:
        block.visit(cb)
    except Exception:
        pass
    return out


def _location(sym: Any, sm: Optional[SourceManager]) -> Optional[str]:
    if sm is None:
        return None
    loc = getattr(sym, "location", None)
    if loc is None:
        return None
    try:
        return f"{sm.getFileName(loc)}:{sm.getLineNumber(loc)}"
    except Exception:
        return None


def lint_compilation(
    comp: Compilation, sm: Optional[SourceManager] = None
) -> list[Finding]:
    """Report symbols written by ``always_ff`` together with any other process."""
    root = comp.getRoot()
    # path -> {"kinds": [procedure kinds], "symbol": value symbol}
    writers: dict[str, dict[str, Any]] = {}
    for top in root.topInstances:
        for block in _walk_blocks(top):
            kind = _proc_kind(block)
            for path, sym in _written_symbols(block).items():
                rec = writers.setdefault(path, {"kinds": [], "symbol": sym})
                rec["kinds"].append(kind)

    findings: list[Finding] = []
    for path, rec in writers.items():
        kinds: list[str] = rec["kinds"]
        # The always_ff single-driver rule: illegal if an always_ff block writes
        # the symbol AND any other process (initial/$readmem, always, a second
        # always_ff, ...) writes it too.
        if "AlwaysFF" in kinds and len(kinds) > 1:
            others = sorted({k for k in kinds if k != "AlwaysFF"})
            findings.append(
                Finding(path=path, others=others, location=_location(rec["symbol"], sm))
            )
    findings.sort(key=lambda f: f.path)
    return findings


def _build_compilation(
    files: list[str], top: Optional[str], include_dirs: Optional[list[str]]
) -> tuple[Compilation, SourceManager]:
    opts = CompilationOptions()
    if top:
        opts.topModules = {top}
    sm = SourceManager()
    # pyslang's addUserDirectories takes one path per call, not a list.
    for inc in include_dirs or []:
        sm.addUserDirectories(inc)
    comp = Compilation(Bag([opts]))
    comp.addSyntaxTree(SyntaxTree.fromFiles(files, sm))
    return comp, sm


def lint_files(
    files: list[str],
    top: Optional[str] = None,
    include_dirs: Optional[list[str]] = None,
) -> list[Finding]:
    comp, sm = _build_compilation(files, top, include_dirs)
    return lint_compilation(comp, sm)


def lint_source(text: str, top: Optional[str] = None) -> list[Finding]:
    """Lint an in-memory snippet -- convenient for tests."""
    opts = CompilationOptions()
    if top:
        opts.topModules = {top}
    sm = SourceManager()
    comp = Compilation(Bag([opts]))
    comp.addSyntaxTree(SyntaxTree.fromText(text, sm))
    return lint_compilation(comp, sm)


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(
        description="Lint SV for illegal always_ff procedural-driver combinations."
    )
    ap.add_argument("files", nargs="*", help="SystemVerilog source files.")
    ap.add_argument("--top", help="Top module name (recommended).")
    ap.add_argument(
        "-f",
        "--filelist",
        action="append",
        default=[],
        metavar="FILE",
        help="EDA-style filelist of sources/+incdir+/-I/-f (repeatable).",
    )
    ap.add_argument(
        "-I",
        "--include",
        action="append",
        default=[],
        metavar="DIR",
        help="Add an include directory (repeatable).",
    )
    args = ap.parse_args(argv)

    files: list[str] = list(args.files)
    include_dirs: list[str] = list(args.include)
    seen: set[str] = set()
    for fl in args.filelist:
        parse_filelist(fl, files, include_dirs, seen)

    if not files:
        ap.error("no source files given (pass files and/or -f FILELIST)")

    findings = lint_files(files, args.top, include_dirs)
    for f in findings:
        print(f"error: {f.message()}", file=sys.stderr)
    if findings:
        print(f"lint: {len(findings)} illegal driver combination(s)", file=sys.stderr)
        return 1
    print("lint: no illegal always_ff driver combinations", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
