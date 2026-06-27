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
        # (instance node id, InstanceSymbol) collected for edge extraction.
        self.instances: list[tuple[int, Any]] = []
        # (logic node id, symbol, role) for process-level logic-edge extraction —
        # inferred registers (`ff`) and combinational blocks (`comb`).
        self.logic_blocks: list[tuple[int, Any, str]] = []

    # -- source-range helpers ------------------------------------------------
    def _file_id(self, path: str) -> int:
        # Store paths with forward slashes so the golden is byte-identical across
        # OSes (slang yields the platform separator); a no-op on POSIX.
        path = path.replace("\\", "/")
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
            "dir": None,
            "const": None,
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
            if kind == "Port":
                # Declared direction, so even unconnected pins land on the right
                # side of the schematic (inputs left, outputs right).
                d = str(getattr(sym, "direction", "")).split(".")[-1]
                node["dir"] = {"In": "in", "Out": "out", "InOut": "inout"}.get(d)
        self.nodes.append(node)
        if parent is not None:
            self.nodes[parent]["children"].append(nid)
        return nid

    @staticmethod
    def _has_edge(timing: Any) -> bool:
        """True if a timing control is edge-sensitive (`posedge`/`negedge`/both) —
        i.e. a clocked process — vs level-sensitive (`@*`, `@(a or b)`)."""
        found = {"v": False}

        def cb(n: Any) -> None:
            e = getattr(n, "edge", None)
            if e is not None and str(e).split(".")[-1] in ("PosEdge", "NegEdge", "BothEdges"):
                found["v"] = True

        if timing is not None:
            try:
                timing.visit(cb)
            except Exception:
                pass
        return found["v"]

    @staticmethod
    def _logic_role(sym: Any) -> Optional[str]:
        """Classify a process / continuous assign as a logic spine node:
        ``'ff'`` (edge-sensitive sequential), ``'comb'`` (combinational/latch), or
        ``None`` (not logic — e.g. ``initial``/``final``).
        """
        kname = _kind_name(sym)
        if kname == "ContinuousAssign":
            return "comb"
        if kname != "ProceduralBlock":
            return None
        pk = str(getattr(sym, "procedureKind", "")).split(".")[-1]
        if pk == "AlwaysFF":
            return "ff"
        if pk in ("AlwaysComb", "AlwaysLatch"):
            return "comb"
        if pk == "Always":
            # Legacy `always`: edge-sensitive ⇒ sequential, else combinational.
            timing = getattr(getattr(sym, "body", None), "timing", None)
            return "ff" if Elaborator._has_edge(timing) else "comb"
        return None  # Initial / Final / other

    def _add_logic(self, sym: Any, parent: Optional[int], role: str) -> int:
        """Emit a process-level logic node — an inferred register (``ff``) or a
        combinational block (``comb``). Processes / continuous assigns are unnamed
        and have no hierarchical path, so synthesize one (``$ff{nid}``/``$comb{nid}``).
        ``def_range`` comes from ``sym.syntax`` (via ``_add``), giving source
        cross-probe for both block kinds for free.
        """
        kind = "FF" if role == "ff" else "Comb"
        tag = "ff" if role == "ff" else "comb"
        nid = self._add(sym, kind, parent)
        n = self.nodes[nid]
        n["name"] = "FF" if role == "ff" else "comb"
        base = self.nodes[parent]["path"] if parent is not None else ""
        n["path"] = f"{base}.${tag}{nid}"
        n["symbol_key"] = n["path"]
        return nid

    @staticmethod
    def _value_refs(node: Any) -> set[str]:
        """Hierarchical paths of every value symbol referenced in an AST subtree."""
        out: set[str] = set()

        def cb(n: Any) -> None:
            k = _kind_name(n)
            if "NamedValue" in k or "HierarchicalValue" in k:
                p = getattr(getattr(n, "symbol", None), "hierarchicalPath", None)
                if p:
                    out.add(p)

        try:
            node.visit(cb)
        except Exception:
            pass
        return out

    @staticmethod
    def _const_bit(expr: Any) -> Optional[str]:
        """Elaborated constant value of a select bound (`gi` -> `0`), else None.
        Only constant bounds yield a select; a variable index resolves to no bit."""
        try:
            c = expr.constant
        except Exception:
            return None
        if c is None or getattr(c, "bad", False):
            return None
        s = str(c)
        return s if s and s != "None" else None

    @staticmethod
    def _base_path(node: Any) -> Optional[str]:
        """Hierarchical path of the symbol an ElementSelect/RangeSelect indexes."""
        val = getattr(node, "value", None)
        if val is None:
            return None
        p = getattr(getattr(val, "symbol", None), "hierarchicalPath", None)
        if p:
            return p
        getref = getattr(val, "getSymbolReference", None)
        if callable(getref):
            try:
                return getattr(getref(), "hierarchicalPath", None)
            except Exception:
                return None
        return None

    def _select_suffix(self, node: Any) -> Optional[str]:
        """Bit-select suffix for an ElementSelect/RangeSelect node using its
        *elaborated constant* bounds: `[0]` or `[7:4]`. None when not constant."""
        k = _kind_name(node)
        if "ElementSelect" in k:
            idx = self._const_bit(getattr(node, "selector", None))
            return f"[{idx}]" if idx is not None else None
        if "RangeSelect" in k:
            msb = self._const_bit(getattr(node, "left", None))
            lsb = self._const_bit(getattr(node, "right", None))
            if msb is not None and lsb is not None:
                return f"[{msb}:{lsb}]"
        return None

    def _selects_in(self, node: Any) -> dict[str, str]:
        """Map base-signal path -> resolved bit-select for every constant index
        in an expression/statement subtree. A signal indexed two different ways
        in the same subtree is dropped (ambiguous -> bare label, no guess)."""
        sel: dict[str, str] = {}
        conflict: set[str] = set()

        def cb(n: Any) -> None:
            k = _kind_name(n)
            if "ElementSelect" in k or "RangeSelect" in k:
                base = self._base_path(n)
                suf = self._select_suffix(n)
                if base and suf:
                    if sel.get(base, suf) != suf:
                        conflict.add(base)
                    sel[base] = suf

        try:
            node.visit(cb)
        except Exception:
            pass
        for b in conflict:
            sel.pop(b, None)
        return sel

    def _logic_edges(self, logic_id: int, sym: Any, role: str, seen: set) -> None:
        """Wire a logic block: data (and, for an FF, clock) signals in, assigned
        signals out. ``data = reads − assigned − clock`` ⇒ a block never reads its
        own output, so `q <= q + 1` produces no self-loop at source."""
        # A continuous assign exposes its `.assignment` expression; procedural
        # blocks are visited directly (their body holds the statements).
        root = getattr(sym, "assignment", None) or sym
        clock: set[str] = set()
        if role == "ff":
            clock = self._value_refs(getattr(getattr(sym, "body", None), "timing", None))
        assigned: set[str] = set()

        def cb(n: Any) -> None:
            if "Assignment" in _kind_name(n):
                left = getattr(n, "left", None)
                if left is not None:
                    assigned.update(self._value_refs(left))

        try:
            root.visit(cb)
        except Exception:
            pass
        data = self._value_refs(root) - assigned - clock
        # Resolved bit-selects (e.g. `core_trap[gi]` -> `[0]`) for the signals
        # this block touches, so each wire is labelled with the bit it carries.
        selects = self._selects_in(root)

        def wire(paths: set, direction: str) -> list[int]:
            ids = []
            for p in sorted(paths):
                nid = self._pick_node(p, ("Net", "Var", "Port"))
                # Only wire real signals — drop genvars/params/enum constants.
                if (
                    nid is not None
                    and nid != logic_id
                    and self.nodes[nid]["kind"] in ("Net", "Var", "Port")
                ):
                    seen.add((logic_id, nid, direction, selects.get(p)))
                    ids.append(nid)
            return ids

        clk_ids = wire(clock, "in")
        wire(data, "in")
        wire(assigned, "out")
        # Tell the renderer which pin is the clock (draws the FF clock notch).
        if clk_ids:
            self.nodes[logic_id]["type"] = self.nodes[clk_ids[0]]["name"]

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

        # A process / continuous assign becomes a logic spine node: an inferred
        # register (`ff`) or a combinational block (`comb`). Its read/assigned (and
        # clock) wiring is recovered later in `_logic_edges`.
        role = self._logic_role(sym)
        if role is not None:
            logic_id = self._add_logic(sym, parent, role)
            self.logic_blocks.append((logic_id, sym, role))
            return

        # Skip the auto-generated internal variable backing a port (the Port node
        # already represents that signal), to avoid duplicate same-path nodes.
        if kind == "Var" and getattr(sym, "isCompilerGenerated", False):
            return

        my_id = parent
        if kind is not None:
            my_id = self._add(sym, kind, parent)
            if kind == "Instance":
                self.instances.append((my_id, sym))

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

    # -- connectivity --------------------------------------------------------
    def _expr_refs(self, expr: Any) -> list[str]:
        """Canonical paths of the symbol(s) a port-connection expression refers to.

        Uses slang's blessed resolution — the expression's `symbol` and
        `getSymbolReference()` — which covers named/hierarchical values, member
        access (interface signals), and simple assignment/select connections.
        Compound expressions (concat/slice mixes) are not decomposed; such a
        connection yields no edge rather than a non-deterministic guess.
        """
        out: list[str] = []
        if expr is None:
            return out
        path = getattr(getattr(expr, "symbol", None), "hierarchicalPath", None)
        if path:
            out.append(path)
        getref = getattr(expr, "getSymbolReference", None)
        if callable(getref):
            try:
                path = getattr(getref(), "hierarchicalPath", None)
            except Exception:
                path = None
            if path:
                out.append(path)
        return out

    def _const_str(self, expr: Any) -> Optional[str]:
        """The literal value of a constant-tied connection (`.irq(32'd0)`), else
        None. Used to annotate inputs driven by a constant rather than a net."""
        try:
            c = expr.constant
        except Exception:
            return None
        if c is None or getattr(c, "bad", False):
            return None
        s = str(c)
        return s if s and s != "None" else None

    def _pick_node(self, path: str, prefer: tuple[str, ...]) -> Optional[int]:
        """Resolve a path to a node id, preferring the given kinds in order."""
        ids = self._by_path.get(path)
        if not ids:
            return None
        for want in prefer:
            for nid, kind in ids:
                if kind == want:
                    return nid
        return ids[0][0]

    def _edges(self) -> list[dict[str, Any]]:
        # path -> [(id, kind)] for resolving connection endpoints.
        self._by_path: dict[str, list[tuple[int, str]]] = {}
        for n in self.nodes:
            self._by_path.setdefault(n["path"], []).append((n["id"], n["kind"]))

        dir_map = {"In": "in", "Out": "out", "InOut": "inout"}
        # Collect as a set keyed by (port, endpoint, dir, select), then sort, so
        # the output is canonical regardless of pyslang container iteration order.
        # `select` is the resolved bit-select (e.g. `[0]`) or None for the whole
        # signal.
        seen_edges: set[tuple[int, int, str, Optional[str]]] = set()
        for inst_id, sym in self.instances:
            conns = getattr(sym, "portConnections", None)
            if conns is None:
                continue
            for c in conns:
                port = getattr(c, "port", None)
                expr = getattr(c, "expression", None)
                if port is None or expr is None:
                    continue
                port_path = getattr(port, "hierarchicalPath", None)
                port_id = self._pick_node(port_path, ("Port", "Var", "Net")) if port_path else None
                # Interface ports have no leaf node; anchor the wire to the box.
                if port_id is None:
                    port_id = inst_id
                direction = dir_map.get(
                    str(getattr(port, "direction", "")).split(".")[-1], "inout"
                )
                refs = self._expr_refs(expr)
                selects = self._selects_in(expr)
                for rp in refs:
                    end_id = self._pick_node(rp, ("Net", "Var", "Port", "Instance"))
                    if end_id is not None and end_id != port_id:
                        seen_edges.add((port_id, end_id, direction, selects.get(rp)))
                # Input tied to a literal (no net): record the constant on the
                # port so the schematic can show its driver (e.g. 32'd0).
                if not refs and port_id != inst_id:
                    lit = self._const_str(expr)
                    if lit is not None and self.nodes[port_id]["kind"] == "Port":
                        self.nodes[port_id]["const"] = lit

        for logic_id, logic_sym, role in self.logic_blocks:
            self._logic_edges(logic_id, logic_sym, role, seen_edges)

        edges: list[dict[str, Any]] = []
        ordered = sorted(seen_edges, key=lambda t: (t[0], t[1], t[2], t[3] or ""))
        for i, (p, e, d, s) in enumerate(ordered):
            edge: dict[str, Any] = {"id": i, "port": p, "endpoint": e, "dir": d}
            if s is not None:
                edge["select"] = s
            edges.append(edge)
        return edges

    def build(self) -> dict[str, Any]:
        root = self.comp.getRoot()
        for top in root.topInstances:
            self._walk(top, None)
        edges = self._edges()
        return {
            "schema_version": SCHEMA_VERSION,
            "design": root.topInstances[0].name if root.topInstances else "",
            "generator": {"tool": "pyslang", "version": pyslang.__version__},
            "files": self.files,
            "nodes": self.nodes,
            "edges": edges,
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
