"""Elaborate a SystemVerilog design with pyslang and emit the Node-model JSON.

Phase 0 scope: emit the *structural spine* — Instances, generate blocks, and the
signal leaves (ports / nets / variables) — with canonical paths, source ranges,
and a stable ``symbol_key``. Connectivity (``drivers``/``loads``) and the
waveform index are Phase 1.

Usage:
    python -m svxprobe_elaborate.elaborate --top picorv32_soc \\
        fixtures/picorv32_soc/rtl/*.sv -o out.json

    # or drive it from one or more EDA-style filelists:
    python -m svxprobe_elaborate.elaborate --top core -f rtl.f -f extra.f -o out.json
"""
from __future__ import annotations

import argparse
import json
import os
import shlex
import sys
from typing import Any, Optional

import pyslang
from pyslang import Bag, SourceManager
from pyslang.ast import Compilation, CompilationOptions
from pyslang.syntax import SyntaxTree

SCHEMA_VERSION = 1

# slang SymbolKind name -> Node-model kind. Anything not in this map is not a
# spine node (parameters, genvars, modports, procedural blocks, imports, ...);
# we still descend through scopes that can contain spine nodes.
_KIND_MAP = {
    "Instance": "Instance",
    "GenerateBlock": "GenBlock",
    "GenerateBlockArray": "GenBlock",
    "Net": "Net",
    "Port": "Port",
    "Variable": "Var",
    # Parameters/localparams are emitted so the matcher can distinguish a
    # simulator-dumped parameter (not a real signal) from an unmatched signal,
    # independent of whether the trace format tags it as a parameter.
    "Parameter": "Param",
}


def _kind_name(sym: Any) -> str:
    return str(sym.kind).replace("SymbolKind.", "")


class Elaborator:
    """Drives a pyslang Compilation and serializes its hierarchy."""

    def __init__(
        self,
        files: list[str],
        top: Optional[str] = None,
        include_dirs: Optional[list[str]] = None,
    ) -> None:
        opts = CompilationOptions()
        if top:
            opts.topModules = {top}
        # Share one SourceManager so `\`include` directives resolve against the
        # user-supplied include dirs (needed for real cores).
        self.sm = SourceManager()
        if include_dirs:
            self.sm.addUserDirectories(include_dirs)
        self.comp = Compilation(Bag([opts]))
        self.comp.addSyntaxTree(SyntaxTree.fromFiles(files, self.sm))
        self.nodes: list[dict[str, Any]] = []
        self._file_ids: dict[str, int] = {}
        self.files: list[dict[str, Any]] = []

    # -- source-range helpers ------------------------------------------------
    def _file_id(self, path: str) -> int:
        fid = self._file_ids.get(path)
        if fid is None:
            fid = len(self.files)
            self._file_ids[path] = fid
            self.files.append({"id": fid, "path": path})
        return fid

    def _loc(self, sl: Any) -> dict[str, int]:
        return {
            "line": self.sm.getLineNumber(sl),
            "col": self.sm.getColumnNumber(sl),
            "offset": sl.offset,
        }

    def _range_from_syntax(self, sym: Any) -> Optional[dict[str, Any]]:
        syntax = getattr(sym, "syntax", None)
        if syntax is None:
            return None
        try:
            sr = syntax.sourceRange
        except Exception:
            return None
        return {
            "file": self._file_id(self.sm.getFileName(sr.start)),
            "start": self._loc(sr.start),
            "end": self._loc(sr.end),
        }

    def _point_range(self, sl: Any) -> Optional[dict[str, Any]]:
        try:
            loc = self._loc(sl)
        except Exception:
            return None
        return {"file": self._file_id(self.sm.getFileName(sl)), "start": loc, "end": loc}

    # -- node emission -------------------------------------------------------
    def _add(self, sym: Any, kind: str, parent: Optional[int]) -> int:
        nid = len(self.nodes)
        path = getattr(sym, "hierarchicalPath", "") or ""
        node: dict[str, Any] = {
            "id": nid,
            "kind": kind,
            "name": getattr(sym, "name", "") or "",
            "path": path,
            "parent": parent,
            "children": [],
            "symbol_key": path,
            "def_range": None,
            "inst_range": None,
            "type": None,
            "drivers": [],
            "loads": [],
        }
        if kind == "Instance":
            # def_range = the module definition; inst_range = the instantiation site.
            defn = getattr(getattr(sym, "body", None), "definition", None)
            if defn is not None:
                node["def_range"] = self._range_from_syntax(defn) or self._point_range(
                    defn.location
                )
            node["inst_range"] = self._range_from_syntax(sym)
        else:
            node["def_range"] = self._range_from_syntax(sym)
            if kind in ("Net", "Port", "Var"):
                t = getattr(sym, "type", None)
                if t is not None:
                    node["type"] = str(t)
        self.nodes.append(node)
        if parent is not None:
            self.nodes[parent]["children"].append(nid)
        return nid

    def _members(self, sym: Any):
        body = getattr(sym, "body", None)
        if body is not None:
            return body
        if getattr(sym, "isScope", False):
            return sym
        return None

    def _walk(self, sym: Any, parent: Optional[int]) -> None:
        kname = _kind_name(sym)
        kind = _KIND_MAP.get(kname)

        # Skip the auto-generated internal variable backing a port (the Port node
        # already represents that signal), to avoid duplicate same-path nodes.
        if kind == "Var" and getattr(sym, "isCompilerGenerated", False):
            return

        my_id = parent
        if kind is not None:
            my_id = self._add(sym, kind, parent)

        members = self._members(sym)
        if members is None:
            return
        # Only descend through scopes that can hold spine nodes.
        try:
            iterator = list(members)
        except TypeError:
            return
        for child in iterator:
            self._walk(child, my_id if kind is not None else parent)

    def build(self) -> dict[str, Any]:
        root = self.comp.getRoot()
        for top in root.topInstances:
            self._walk(top, None)
        return {
            "schema_version": SCHEMA_VERSION,
            "design": root.topInstances[0].name if root.topInstances else "",
            "generator": {"tool": "pyslang", "version": pyslang.__version__},
            "files": self.files,
            "nodes": self.nodes,
        }


def _resolve(base: str, path: str) -> str:
    return path if os.path.isabs(path) else os.path.normpath(os.path.join(base, path))


def parse_filelist(
    path: str,
    files: list[str],
    include_dirs: list[str],
    seen: Optional[set[str]] = None,
) -> None:
    """Parse an EDA-style filelist, appending sources and include dirs.

    Supports the common subset: source paths, ``+incdir+DIR`` (``+``-separated),
    ``-I DIR`` / ``-IDIR``, nested ``-f FILE``, ``//`` and ``#`` comments. Paths
    are resolved relative to the filelist's own directory (portable across
    invocation cwds). Other directives are ignored.
    """
    seen = seen if seen is not None else set()
    ap = os.path.abspath(path)
    if ap in seen:
        return
    seen.add(ap)
    base = os.path.dirname(ap)

    with open(ap) as fh:
        for raw in fh:
            line = raw.split("//", 1)[0].strip()
            if not line or line.startswith("#"):
                continue
            toks = shlex.split(line)
            i = 0
            while i < len(toks):
                tok = toks[i]
                if tok == "-f":
                    parse_filelist(_resolve(base, toks[i + 1]), files, include_dirs, seen)
                    i += 2
                elif tok.startswith("-f"):
                    parse_filelist(_resolve(base, tok[2:]), files, include_dirs, seen)
                    i += 1
                elif tok == "-I":
                    include_dirs.append(_resolve(base, toks[i + 1]))
                    i += 2
                elif tok.startswith("-I"):
                    include_dirs.append(_resolve(base, tok[2:]))
                    i += 1
                elif tok.startswith("+incdir+"):
                    for d in tok[len("+incdir+") :].split("+"):
                        if d:
                            include_dirs.append(_resolve(base, d))
                    i += 1
                elif tok.startswith(("+", "-")):
                    i += 1  # unsupported directive; ignore
                else:
                    files.append(_resolve(base, tok))
                    i += 1


def build_model(
    files: list[str],
    top: Optional[str] = None,
    include_dirs: Optional[list[str]] = None,
) -> dict[str, Any]:
    return Elaborator(files, top, include_dirs).build()


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description="Elaborate SV and emit Node-model JSON.")
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
    ap.add_argument("-o", "--out", default="-", help="Output path ('-' = stdout).")
    args = ap.parse_args(argv)

    files: list[str] = list(args.files)
    include_dirs: list[str] = list(args.include)
    seen: set[str] = set()
    for fl in args.filelist:
        parse_filelist(fl, files, include_dirs, seen)

    if not files:
        ap.error("no source files given (pass files and/or -f FILELIST)")

    model = build_model(files, args.top, include_dirs)
    text = json.dumps(model, indent=2)
    if args.out == "-":
        sys.stdout.write(text + "\n")
    else:
        with open(args.out, "w") as f:
            f.write(text + "\n")
    print(
        f"elaborated {model['design']}: {len(model['nodes'])} nodes, "
        f"{len(model['files'])} files",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
