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
import re
import shlex
import sys
from typing import Any, Optional

import pyslang
from pyslang import Bag, SourceManager
from pyslang.analysis import AnalysisManager
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
    # An interface's modport (a named view of the bundle). A modport-specialized
    # interface port on a consumer (`mem_if.mem bus`) is an `InterfacePort`, which
    # we model as an `Interface` node carrying the selected view (see `_add`).
    # ModportPort symbols stay non-spine in general (views of existing signals),
    # with one exception: on a modport-specialized interface port every member has
    # one concrete direction, so they emit as that node's directional `Port`
    # children (see `_add_modport_pins`).
    "Modport": "Modport",
    "InterfacePort": "Interface",
}


def _is_interface_instance(sym: Any) -> bool:
    """True if `sym` is an instance whose definition is an `interface` (not a
    module) — so it can be retagged from the generic `Instance` to `Interface`."""
    defn = getattr(getattr(sym, "body", None), "definition", None)
    dk = str(getattr(defn, "definitionKind", "")).split(".")[-1]
    return dk == "Interface"


def _kind_name(sym: Any) -> str:
    return str(sym.kind).replace("SymbolKind.", "")


def _op_name(expr: Any) -> str:
    """Bare operator name of a Binary/Unary expression (`BinaryAnd`, `BitwiseNot`,
    …). slang prints it as ``BinaryOperator.BinaryAnd``; take the last segment."""
    return str(getattr(expr, "op", "")).split(".")[-1]


class _NetInitializer:
    """Presents a net declaration initializer (``wire w = expr;``) to
    ``_logic_edges`` as a continuous assign (#110): ``assignment`` is the RHS
    expression and ``target_path`` the initialized net's canonical path — its
    l-value, which the RHS-only expression tree cannot name itself."""

    def __init__(self, expr: Any, target_path: str) -> None:
        self.assignment = expr
        self.target_path = target_path


def _value_sym_path(sym: Any) -> Optional[str]:
    """Canonical path of a referenced value symbol. A ModportPort is slang's view
    of an interface member as seen through a modport-qualified port (its own path,
    e.g. `...bus.mem.valid`, names no model node); resolve it to the underlying
    signal via slang's `internalSymbol` link — a model lookup, not a name guess.
    Returns None for a ModportPort with no resolvable member (never guess)."""
    if sym is None:
        return None
    if _kind_name(sym) == "ModportPort":
        sym = getattr(sym, "internalSymbol", None)
    return getattr(sym, "hierarchicalPath", None) if sym is not None else None


def _parse_sv_int(rhs: str) -> Optional[int]:
    """Parse a SystemVerilog integer literal (`2'd2`, `4'hA`, `3'b001`, `5`) to an
    int. Returns None for non-literal initializers (computed expressions) or values
    with x/z bits — those are not nameable constants, and we never guess."""
    s = rhs.strip().replace("_", "")
    m = re.fullmatch(r"(?:\d+)?'[sS]?([dDhHbBoO])([0-9a-fA-FxXzZ?]+)", s)
    if m:
        if re.search(r"[xXzZ?]", m.group(2)):
            return None
        base = {"d": 10, "h": 16, "b": 2, "o": 8}[m.group(1).lower()]
        try:
            return int(m.group(2), base)
        except ValueError:
            return None
    if re.fullmatch(r"[+-]?\d+", s):
        return int(s)
    return None


def _enum_members(enum_type: Any) -> Optional[list[dict[str, Any]]]:
    """`[{name, value}]` for an enum type, using SystemVerilog semantics: an explicit
    literal initializer, else the positional default (first = 0, otherwise prev + 1).
    Returns None if any member has a non-literal initializer (so we never invent a
    value)."""
    out: list[dict[str, Any]] = []
    nxt = 0
    for mem in enum_type:
        syn = getattr(mem, "syntax", None)
        text = str(syn) if syn is not None else getattr(mem, "name", "")
        if "=" in text:
            value = _parse_sv_int(text.split("=", 1)[1])
            if value is None:
                return None
        else:
            value = nxt
        out.append({"name": getattr(mem, "name", ""), "value": value})
        nxt = value + 1
    return out


class Elaborator:
    """Drives a pyslang Compilation and serializes its hierarchy."""

    def __init__(
        self,
        files: list[str],
        top: Optional[str] = None,
        include_dirs: Optional[list[str]] = None,
        gate_level: bool = False,
    ) -> None:
        # Opt-in gate-level projection (#157, ADR 0005): decompose process/assign
        # RHS expressions into gate/mux primitive nodes *in addition* to the
        # process-level logic nodes. Off by default ⇒ output byte-identical.
        self.gate_level = gate_level
        opts = CompilationOptions()
        if top:
            opts.topModules = {top}
        # Share one SourceManager so `\`include` directives resolve against the
        # user-supplied include dirs (needed for real cores).
        self.sm = SourceManager()
        # pyslang's addUserDirectories takes one path per call, not a list.
        for inc in include_dirs or []:
            self.sm.addUserDirectories(inc)
        self.comp = Compilation(Bag([opts]))
        self.comp.addSyntaxTree(SyntaxTree.fromFiles(files, self.sm))
        self.nodes: list[dict[str, Any]] = []
        # Normalized enum table: canonical type string -> {width, members}. Filled as
        # enum-typed signals are walked; referenced by node["type"] (#81).
        self.enums: dict[str, dict[str, Any]] = {}
        self._file_ids: dict[str, int] = {}
        self.files: list[dict[str, Any]] = []
        # (instance node id, InstanceSymbol) collected for edge extraction.
        self.instances: list[tuple[int, Any]] = []
        # (pin node id, underlying member path, dir) for each modport member pin,
        # collected for edge extraction (the pin wires to its bundle member).
        self.modport_pins: list[tuple[int, str, str]] = []
        # Their ids: a pin is a *view* of its member, never the member itself,
        # so path resolution (`_pick_node`) must not land on a pin while a real
        # signal exists at the same path.
        self._modport_pin_ids: set[int] = set()
        # (logic node id, symbol, role) for process-level logic-edge extraction —
        # inferred registers (`ff`) and combinational blocks (`comb`).
        self.logic_blocks: list[tuple[int, Any, str]] = []
        # `initial` blocks collected for the $readmemh INIT-marker pass (#112).
        # `initial` stays non-logic (ADR 0004); we only scan it to attribute a
        # `$readmem*` initializer to the memory array it targets.
        self.init_blocks: list[Any] = []
        # Source locations slang's analysis flags as inferring a latch (set in build).
        self._inferred_latch_locs: set[tuple[Any, int]] = set()

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

    def _range_from_expr(self, expr: Any) -> Optional[dict[str, Any]]:
        """`def_range` for a gate-level primitive: the *sub-expression's* own
        ``sourceRange`` (a span within a line, e.g. the ``a & b`` fragment). This
        is the one scoped relaxation of ADR 0004's "one box = one source
        construct" — sanctioned for the opt-in gate-level projection by ADR 0005 —
        so clicking a gate still cross-probes to source by lookup, never a guess."""
        try:
            sr = expr.sourceRange
            return {
                "file": self._file_id(self.sm.getFileName(sr.start)),
                "start": self._loc(sr.start),
                "end": self._loc(sr.end),
            }
        except Exception:
            return None

    # -- node emission -------------------------------------------------------
    def _add(
        self, sym: Any, kind: str, parent: Optional[int], path: Optional[str] = None
    ) -> int:
        """Emit a node for `sym`. `path` overrides the node's canonical identity
        (path + symbol_key) when the symbol is a *view* of another signal — a
        modport pin carries the path of the member it exposes."""
        nid = len(self.nodes)
        if path is None:
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
            "modport": None,
            "drivers": [],
            "loads": [],
        }
        # A modport-specialized interface port records the view it selects (e.g.
        # `mem_if.mem bus` -> "mem"); a plain interface instance leaves it null.
        mp = getattr(sym, "modport", None)
        if isinstance(mp, str) and mp:
            node["modport"] = mp
        if kind in ("Instance", "Interface"):
            # def_range = the module/interface definition; inst_range = the
            # instantiation site. A consuming interface *port* has no body; its
            # definition is the interface type itself (`interfaceDef`).
            defn = getattr(getattr(sym, "body", None), "definition", None)
            if defn is None:
                defn = getattr(sym, "interfaceDef", None)
            if defn is not None:
                node["def_range"] = self._range_from_syntax(defn) or self._point_range(
                    defn.location
                )
            node["inst_range"] = self._range_from_syntax(sym)
        else:
            node["def_range"] = self._range_from_syntax(sym)
            if kind in ("Net", "Port", "Var", "Memory"):
                t = getattr(sym, "type", None)
                if t is not None:
                    node["type"] = str(t)
                    self._record_enum(str(t), t)
            if kind == "Port":
                # Declared direction, so even unconnected pins land on the right
                # side of the schematic (inputs left, outputs right).
                d = str(getattr(sym, "direction", "")).split(".")[-1]
                node["dir"] = {"In": "in", "Out": "out", "InOut": "inout"}.get(d)
            if kind == "Modport":
                # Per-modport membership + directions as model data (#95). The
                # modport stays a view (no children/drivers/loads); this is
                # descriptive metadata for schematic modport rendering.
                node["members"] = self._modport_members(sym)
        self.nodes.append(node)
        if parent is not None:
            self.nodes[parent]["children"].append(nid)
        return nid

    @staticmethod
    def _modport_members(sym: Any) -> list[dict[str, str]]:
        """Membership of a modport view: each bundle member visible through it,
        with its direction — straight from slang's ``ModportPort`` (``direction``
        + ``internalSymbol``, no name heuristics). The member's own node lives in
        the parent interface instance at ``<parent path>.<name>``; members whose
        underlying signal slang cannot resolve are skipped (never guess)."""
        out: list[dict[str, str]] = []
        for mp in sym:
            if _kind_name(mp) != "ModportPort":
                continue
            if getattr(mp, "internalSymbol", None) is None:
                continue
            d = str(getattr(mp, "direction", "")).split(".")[-1]
            out.append(
                {
                    "name": getattr(mp, "name", "") or "",
                    "dir": {"In": "in", "Out": "out", "InOut": "inout"}.get(d, "inout"),
                }
            )
        return out

    def _add_modport_pins(self, sym: Any, parent: int) -> None:
        """Directional member pins for a modport-specialized interface port.

        Through a named modport view every bundle member has one concrete
        direction, so each of the modport's ``ModportPort``s emits as a ``Port``
        child of the consumer's ``Interface`` node — direction and underlying
        member taken straight from slang (``direction`` / ``internalSymbol``, no
        name heuristics). A pin's path *is* the underlying member's canonical
        path (the pin is a view of that signal), so pin clicks cross-probe to
        the member's source and waveform as plain path lookups; ``_edges`` wires
        each pin to its member like an ordinary port connection. A bare
        interface port (no modport) stays port-less: its members carry both
        directions, so there is nothing unambiguous to pin.
        """
        conn = getattr(sym, "connection", None)
        modport = conn[1] if isinstance(conn, tuple) and len(conn) == 2 else None
        if modport is None:
            return
        for mp in modport:
            if _kind_name(mp) != "ModportPort":
                continue
            mpath = _value_sym_path(mp)
            if not mpath:
                continue  # no underlying member resolved -> no pin; never guess
            nid = self._add(mp, "Port", parent, path=mpath)
            self._modport_pin_ids.add(nid)
            self.modport_pins.append((nid, mpath, self.nodes[nid]["dir"] or "inout"))

    def _record_enum(self, type_str: str, t: Any) -> None:
        """If `t` is (an alias of) an enum, record its value->name members once under
        `type_str` (the same string stored on the node), keyed so the frontend can map
        a signal's value to its state name. Skips enums with non-literal members."""
        if type_str in self.enums:
            return
        try:
            ct = t.canonicalType
        except Exception:
            return
        if not getattr(ct, "isEnum", False):
            return
        members = _enum_members(ct)
        if members is None:
            return
        self.enums[type_str] = {"width": int(getattr(ct, "bitWidth", 0)), "members": members}

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
    def _stmt_kind(stmt: Any) -> str:
        """Bare statement-kind name (`Timed`, `Block`, `Conditional`, …)."""
        return str(getattr(stmt, "kind", "")).split(".")[-1]

    def _gating_condition_refs(self, sym: Any) -> set[str]:
        """Signals read by the single top-level conditional gating a process
        body, peeling timing controls and begin/end blocks structurally. Empty
        when the top level is anything else — no single gating condition means
        no fact (never guess from source text)."""
        stmt = getattr(sym, "body", None)
        while stmt is not None:
            k = self._stmt_kind(stmt)
            if k == "Timed":
                stmt = getattr(stmt, "stmt", None)
            elif k == "Block":
                stmt = getattr(stmt, "body", None)
            elif k == "List":
                items = list(getattr(stmt, "list", None) or [])
                stmt = items[0] if len(items) == 1 else None
            else:
                break
        if stmt is None or self._stmt_kind(stmt) != "Conditional":
            return set()
        refs: set[str] = set()
        for c in getattr(stmt, "conditions", None) or []:
            refs |= self._value_refs(getattr(c, "expr", None))
        return refs

    def _collect_inferred_latches(self) -> set[tuple[Any, int]]:
        """Source locations slang's analysis pass flags as inferring a latch — i.e.
        an ``always_comb`` / ``always_latch`` that holds state on some path. slang
        only reports this for the *no-latch-contract* forms; a legacy level-sensitive
        ``always`` that infers a latch is legal Verilog and is **not** flagged, so it
        stays ``comb`` (we do not second-guess the model with our own heuristic).
        Best-effort: never block elaboration if the analysis API misbehaves."""
        locs: set[tuple[Any, int]] = set()
        try:
            # The analysis pass requires a fully-elaborated compilation.
            self.comp.getAllDiagnostics()
            am = AnalysisManager()
            am.analyze(self.comp)
            for d in am.getDiagnostics():
                if d.code == pyslang.Diags.InferredLatch:
                    loc = d.location
                    locs.add((loc.buffer, loc.offset))
        except Exception:
            pass
        return locs

    def _infers_latch(self, sym: Any) -> bool:
        """True if `sym`'s source range contains a slang `InferredLatch` location."""
        if not self._inferred_latch_locs:
            return False
        try:
            sr = sym.syntax.sourceRange
            start, end = sr.start, sr.end
            return any(
                buf == start.buffer and start.offset <= off <= end.offset
                for (buf, off) in self._inferred_latch_locs
            )
        except Exception:
            return False

    @staticmethod
    def _memory_depth(sym: Any) -> Optional[int]:
        """Word count of a memory array (`logic [W-1:0] ram [0:N-1]` -> N), or
        None if `sym` is not an unpacked array. Structural, from slang's type
        (`type.isUnpackedArray` + the unpacked `range.width`) — never parsed from
        the type string (#112)."""
        t = getattr(sym, "type", None)
        if t is None or not getattr(t, "isUnpackedArray", False):
            return None
        rng = getattr(t, "range", None)
        w = getattr(rng, "width", None) if rng is not None else None
        return int(w) if isinstance(w, int) else None

    @staticmethod
    def _is_initial(sym: Any) -> bool:
        return (
            _kind_name(sym) == "ProceduralBlock"
            and str(getattr(sym, "procedureKind", "")).split(".")[-1] == "Initial"
        )

    def _logic_role(self, sym: Any) -> Optional[str]:
        """Classify a process / continuous assign as a logic spine node:
        ``'ff'`` (edge-sensitive sequential), ``'comb'`` (combinational process —
        ``always_comb`` / ``always @*``), ``'latch'`` (``always_latch`` or an
        ``always_comb`` slang's analysis flags as inferring a latch), ``'assign'``
        (continuous ``assign``), or ``None`` (not logic — e.g. ``initial`` /
        ``final``). ``comb`` and ``assign`` are both combinational but kept distinct
        so the schematic can render a process as a box and an assign as a function
        node.
        """
        kname = _kind_name(sym)
        if kname == "ContinuousAssign":
            return "assign"
        if kname != "ProceduralBlock":
            return None
        pk = str(getattr(sym, "procedureKind", "")).split(".")[-1]
        if pk == "AlwaysFF":
            return "ff"
        if pk == "AlwaysLatch":
            return "latch"
        if pk == "AlwaysComb":
            # Combinational intent — but slang may have found it infers a latch
            # (incomplete assignment), which is the truth we render.
            return "latch" if self._infers_latch(sym) else "comb"
        if pk == "Always":
            # Legacy `always`: edge-sensitive ⇒ sequential, else combinational.
            # (A level-sensitive legacy `always` that infers a latch is legal Verilog
            # and is *not* flagged by slang, so it stays `comb` — only the explicit
            # `always_latch` and a latch-inferring `always_comb` become `latch`.)
            timing = getattr(getattr(sym, "body", None), "timing", None)
            return "ff" if Elaborator._has_edge(timing) else "comb"
        return None  # Initial / Final / other

    # role -> (NodeKind, node name / path tag). The tag also names the synthetic
    # path segment (`$ff12` / `$comb12` / `$assign12`).
    _LOGIC_KIND = {
        "ff": ("FF", "FF", "ff"),
        "comb": ("Comb", "comb", "comb"),
        "latch": ("Latch", "latch", "latch"),
        "assign": ("Assign", "assign", "assign"),
    }

    def _add_logic(self, sym: Any, parent: Optional[int], role: str) -> int:
        """Emit a process-level logic node — an inferred register (``ff``), a
        combinational process (``comb`` — ``always_comb`` / ``always @*``), a level
        latch (``latch`` — ``always_latch``), or a continuous assign (``assign``).
        Processes / continuous assigns are unnamed
        and have no hierarchical path, so synthesize one (``$ff{nid}`` etc.).
        ``def_range`` comes from ``sym.syntax`` (via ``_add``), giving source
        cross-probe for every block kind for free.
        """
        kind, name, tag = self._LOGIC_KIND[role]
        nid = self._add(sym, kind, parent)
        n = self.nodes[nid]
        n["name"] = name
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
                p = _value_sym_path(getattr(n, "symbol", None))
                if p:
                    out.add(p)

        try:
            node.visit(cb)
        except Exception:
            pass
        return out

    def _lvalue_bases(self, node: Any) -> set[str]:
        """Hierarchical paths of the symbols an assignment LHS actually stores
        to (#110). Selects peel to their base — ``ram[idx]`` targets ``ram``,
        the index is a read — member access to its underlying value (a struct
        field store targets the struct; an interface member like ``bus.rdata``
        arrives as a HierarchicalValue, not a MemberAccess), and concatenations
        descend per operand. Unrecognized shapes fall back to every value ref
        in the subtree, over-approximating ``assigned`` (the old behavior)
        rather than silently dropping outputs."""
        if node is None:
            return set()
        k = _kind_name(node)
        if "NamedValue" in k or "HierarchicalValue" in k:
            p = _value_sym_path(getattr(node, "symbol", None))
            return {p} if p else set()
        if "ElementSelect" in k or "RangeSelect" in k or "MemberAccess" in k:
            return self._lvalue_bases(getattr(node, "value", None))
        if "Concatenation" in k and "Streaming" not in k:
            out: set[str] = set()
            for op in getattr(node, "operands", None) or []:
                out |= self._lvalue_bases(op)
            return out
        return self._value_refs(node)

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
        if not s or s == "None":
            return None
        # A computed bound (e.g. `AW+1`) prints as a sized constant (`32'd10`)
        # while a literal prints bare — normalize so select labels are uniform
        # (`[10:2]`, never `[32'd10:2]`).
        m = re.fullmatch(r"\d+'s?d(\d+)", s)
        return m.group(1) if m else s

    @staticmethod
    def _base_path(node: Any) -> Optional[str]:
        """Hierarchical path of the symbol an ElementSelect/RangeSelect indexes."""
        val = getattr(node, "value", None)
        if val is None:
            return None
        p = _value_sym_path(getattr(val, "symbol", None))
        if p:
            return p
        getref = getattr(val, "getSymbolReference", None)
        if callable(getref):
            try:
                return _value_sym_path(getref())
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

    def _mem_accesses(self, node: Any) -> list[tuple[int, set[str]]]:
        """Every memory-array access in a subtree: for each
        ElementSelect/RangeSelect whose base resolves to a ``Memory`` node, that
        memory's id paired with the value refs of its index expression (the
        address). Bounded to array indexing — not general operator walking (#112)."""
        out: list[tuple[int, set[str]]] = []

        def cb(n: Any) -> None:
            k = _kind_name(n)
            is_elem = "ElementSelect" in k
            is_range = "RangeSelect" in k
            if not (is_elem or is_range):
                return
            base = self._base_path(n)
            if not base:
                return
            mid = self._pick_node(base, ("Memory",))
            if mid is None or self.nodes[mid]["kind"] != "Memory":
                return
            if is_elem:
                addr = self._value_refs(getattr(n, "selector", None))
            else:
                addr = self._value_refs(getattr(n, "left", None)) | self._value_refs(
                    getattr(n, "right", None)
                )
            out.append((mid, addr))

        try:
            node.visit(cb)
        except Exception:
            pass
        return out

    def _memory_edges(self, root: Any, seen: set) -> None:
        """Wire each memory accessed in ``root`` to its real addr/din/dout signals
        with typed pins (``mem_port``), so the MEMORY glyph connects to the actual
        scope signals (e.g. ``word_idx`` / ``wdata`` / ``rdata``) rather than the
        process. ``addr``/``din`` are memory inputs (``in``), ``dout`` a memory
        output (``out``). Write/read *enable* pins are deferred (#157) (#112)."""
        assigns: list[Any] = []

        def cb(n: Any) -> None:
            if "Assignment" in _kind_name(n):
                assigns.append(n)

        try:
            root.visit(cb)
        except Exception:
            pass

        def emit(mem_id: int, paths: set[str], direction: str, mem_port: str) -> None:
            for p in sorted(paths):
                nid = self._pick_node(p, ("Net", "Var", "Port"))
                # Only real scalar/bus signals — never another memory (no mem↔mem).
                if (
                    nid is not None
                    and nid != mem_id
                    and self.nodes[nid]["kind"] in ("Net", "Var", "Port")
                ):
                    seen.add((mem_id, nid, direction, None, mem_port, None))

        for a in assigns:
            left = getattr(a, "left", None)
            right = getattr(a, "right", None)
            # Write `ram[idx] <= rhs`: index -> addr, RHS -> din (both memory ins).
            for mem_id, addr in self._mem_accesses(left):
                emit(mem_id, addr, "in", "addr")
                emit(mem_id, self._value_refs(right), "in", "din")
            # Read `lhs <= ram[idx]`: index -> addr, LHS target -> dout (memory out).
            for mem_id, addr in self._mem_accesses(right):
                emit(mem_id, addr, "in", "addr")
                emit(mem_id, self._lvalue_bases(left), "out", "dout")

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
                    assigned.update(self._lvalue_bases(left))

        try:
            root.visit(cb)
        except Exception:
            pass
        # A net declaration initializer's target is not in the expression tree
        # (there is no Assignment node) — the adapter names it directly.
        target = getattr(sym, "target_path", None)
        if target is not None:
            assigned.add(target)
        data = self._value_refs(root) - assigned - clock
        # Resolved bit-selects (e.g. `core_trap[gi]` -> `[0]`) for the signals
        # this block touches, so each wire is labelled with the bit it carries.
        selects = self._selects_in(root)

        def wire(paths: set, direction: str) -> list[int]:
            ids = []
            for p in sorted(paths):
                nid = self._pick_node(p, ("Net", "Var", "Port"))
                # Only wire real signals — drop genvars/params/enum constants, and
                # memories: those are wired to their addr/din/dout signals with
                # typed pins instead (#112, `_memory_edges`), not to the process.
                if (
                    nid is not None
                    and nid != logic_id
                    and self.nodes[nid]["kind"] in ("Net", "Var", "Port")
                ):
                    seen.add((logic_id, nid, direction, selects.get(p), None, None))
                    ids.append(nid)
            return ids

        clk_ids = wire(clock, "in")
        wire(data, "in")
        wire(assigned, "out")
        # Wire any memory this block accesses to its real addr/din/dout signals
        # (word_idx / wdata / rdata), with typed pins — replaces the plain
        # process↔memory edge (#112). Read/write *enable* pins stay deferred (#157).
        self._memory_edges(root, seen)
        # Tell the renderer which pin is the clock (draws the FF clock notch).
        # With several timing signals (async reset) the reset is the event whose
        # signal the body *also reads* (#59) — a structural fact, never a name
        # guess. When that rule disambiguates, `type` names the true clock and
        # `reset` the async-reset's canonical path; otherwise fall back to the
        # first wired timing signal and emit no reset.
        if clk_ids:
            clock_name = self.nodes[clk_ids[0]]["name"]
            if len(clk_ids) > 1:
                body_reads = self._value_refs(
                    getattr(getattr(sym, "body", None), "stmt", None)
                )
                clocks = [i for i in clk_ids if self.nodes[i]["path"] not in body_reads]
                resets = [i for i in clk_ids if self.nodes[i]["path"] in body_reads]
                if len(clocks) == 1:
                    clock_name = self.nodes[clocks[0]]["name"]
                    if len(resets) == 1:
                        self.nodes[logic_id]["reset"] = self.nodes[resets[0]]["path"]
            self.nodes[logic_id]["type"] = clock_name
        # A latch's gate: the sole signal read by the body's top-level
        # conditional and not assigned by the block (#59). Anything else —
        # compound gate, no top-level conditional — emits nothing (no guess).
        if role == "latch":
            gate = self._gating_condition_refs(sym) - assigned
            if len(gate) == 1:
                eid = self._pick_node(next(iter(gate)), ("Net", "Var", "Port"))
                if eid is not None and self.nodes[eid]["kind"] in ("Net", "Var", "Port"):
                    self.nodes[logic_id]["enable"] = self.nodes[eid]["path"]

    # -- gate-level projection (#157, ADR 0005) ------------------------------
    # slang operator name -> primitive NodeKind. Operators that share a glyph and
    # meaning fold together (all comparisons -> Cmp, all shifts -> Shift); the
    # exact operator is preserved on the node's `op` field for the label. Div/Mod/
    # Power render as a labelled Mul-family box (ADR 0005 out-of-scope note).
    _BINOP_KIND = {
        "BinaryAnd": "And", "LogicalAnd": "And",
        "BinaryOr": "Or", "LogicalOr": "Or",
        "BinaryXor": "Xor",
        "BinaryXnor": "Xnor",
        "Add": "Add",
        "Subtract": "Sub",
        "Multiply": "Mul", "Divide": "Mul", "Mod": "Mul", "Power": "Mul",
        "Equality": "Cmp", "Inequality": "Cmp",
        "CaseEquality": "Cmp", "CaseInequality": "Cmp",
        "WildcardEquality": "Cmp", "WildcardInequality": "Cmp",
        "LessThan": "Cmp", "GreaterThan": "Cmp",
        "LessThanEqual": "Cmp", "GreaterThanEqual": "Cmp",
        "LogicalShiftLeft": "Shift", "LogicalShiftRight": "Shift",
        "ArithmeticShiftLeft": "Shift", "ArithmeticShiftRight": "Shift",
    }
    # Associative logic operators whose same-op chain collapses to one N-input gate
    # (`a & b & c` -> one 3-input And). Datapath/compare/shift ops stay binary.
    _ASSOC = {"And", "Or", "Xor", "Xnor"}
    # Reduction unary operators fold into the matching gate kind — a reduction-AND
    # *is* an And gate with one wide input; the negated reductions (`~&`, `~|`,
    # `~^`) carry the output bubble (ADR 0005 §2).
    _REDUCE = {
        "BitwiseAnd": "And", "BitwiseOr": "Or", "BitwiseXor": "Xor",
        "BitwiseNand": "Nand", "BitwiseNor": "Nor", "BitwiseXnor": "Xnor",
    }
    # A `~`/`!` directly wrapping an associative gate chain folds the bubble onto
    # the gate (`~(a & b)` -> one Nand), instead of a redundant inverter on a gate.
    _FOLD = {"And": "Nand", "Or": "Nor", "Xor": "Xnor"}

    def _add_gate(
        self, kind: str, expr: Any, parent: int, op: Optional[str] = None
    ) -> int:
        """Emit a synthesized gate/mux primitive node as a flat child of the logic
        block `parent`. Its path extends `_add_logic`'s scheme with a per-kind
        synthetic segment (`…$and{nid}` / `…$mux{nid}`); `def_range` is the
        sub-expression's own span (`_range_from_expr`)."""
        nid = len(self.nodes)
        base = self.nodes[parent]["path"]
        tag = kind.lower()
        path = f"{base}.${tag}{nid}"
        node: dict[str, Any] = {
            "id": nid,
            "kind": kind,
            "name": tag,
            "path": path,
            "parent": parent,
            "children": [],
            "symbol_key": path,
            "def_range": self._range_from_expr(expr),
            "inst_range": None,
            "type": None,
            "dir": None,
            "const": None,
            "modport": None,
            "drivers": [],
            "loads": [],
        }
        # The exact operator (`Equality`, `LessThan`, `Divide`, `LogicalShiftLeft`,
        # …) so the frontend can label a Cmp/Shift/Mul box, which the coarse kind
        # alone cannot disambiguate.
        if op is not None:
            node["op"] = op
        self.nodes.append(node)
        self.nodes[parent]["children"].append(nid)
        return nid

    def _leaf_signal(self, expr: Any) -> Optional[str]:
        """Canonical path if `expr` reads a single signal (a name, or a
        bit/part/member select of one) — a gate input wired straight to that
        signal. None for a compound expression (which decomposes further).

        The bit-select is peeled to the base signal and *not* carried onto the
        gate input edge (the 6-tuple's `select` slot stays None in `_wire_input`),
        unlike `_logic_edges`' process-level wiring. A gate reading `a[3:0]` thus
        renders wired to whole `a` — a known fidelity simplification of the opt-in
        gate view, deferred to a follow-up slice."""
        if expr is None:
            return None
        k = _kind_name(expr)
        if "NamedValue" in k or "HierarchicalValue" in k:
            return _value_sym_path(getattr(expr, "symbol", None))
        if "ElementSelect" in k or "RangeSelect" in k or "MemberAccess" in k:
            return self._leaf_signal(getattr(expr, "value", None))
        return None

    @staticmethod
    def _flatten_assoc(expr: Any, op: str) -> list[Any]:
        """Operands of a same-operator associative chain: `a & b & c` (parsed as
        `(a & b) & c`) -> [a, b, c]."""
        out: list[Any] = []

        def rec(e: Any) -> None:
            if e is not None and "BinaryOp" in _kind_name(e) and _op_name(e) == op:
                rec(getattr(e, "left", None))
                rec(getattr(e, "right", None))
            else:
                out.append(e)

        rec(expr)
        return out

    def _emit_expr(
        self, expr: Any, logic_id: int, seen: set
    ) -> Optional[tuple[str, Any]]:
        """Decompose `expr` into gate/mux nodes (children of `logic_id`), returning
        the endpoint that carries its result: ``("sig", path)`` for a bare signal,
        ``("node", nid)`` for a synthesized primitive, or None (constant / not
        decomposable) — no wire."""
        if expr is None:
            return None
        k = _kind_name(expr)
        # Implicit width/sign conversions slang inserts are transparent to the view.
        if "Conversion" in k or "Cast" in k:
            return self._emit_expr(getattr(expr, "operand", None), logic_id, seen)
        sig = self._leaf_signal(expr)
        if sig is not None:
            # A named leaf referencing a parameter (#199): keep its node/path for
            # cross-probe but carry the parameter's resolved value too.
            pv = self._param_value(expr)
            if pv is not None:
                return ("param", sig, pv)
            return ("sig", sig)
        if "ConditionalOp" in k:
            return self._emit_mux(expr, logic_id, seen)
        if "BinaryOp" in k:
            op = _op_name(expr)
            kind = self._BINOP_KIND.get(op)
            if kind is None:
                return self._const_input(expr)
            if kind in self._ASSOC:
                operands = self._flatten_assoc(expr, op)
            else:
                operands = [getattr(expr, "left", None), getattr(expr, "right", None)]
            return self._emit_gate(kind, expr, operands, logic_id, seen, op)
        if "UnaryOp" in k:
            op = _op_name(expr)
            operand = getattr(expr, "operand", None)
            if op in ("BitwiseNot", "LogicalNot"):
                fold = self._fold_not(operand)
                if fold is not None:
                    folded_kind, inner_op = fold
                    operands = self._flatten_assoc(operand, inner_op)
                    return self._emit_gate(folded_kind, expr, operands, logic_id, seen, op)
                return self._emit_gate("Not", expr, [operand], logic_id, seen, op)
            red = self._REDUCE.get(op)
            if red is not None:
                return self._emit_gate(red, expr, [operand], logic_id, seen, op)
            if op == "Plus":
                # Unary `+` is identity -> a buffer (bare triangle, ADR 0005 §2).
                return self._emit_gate("Buf", expr, [operand], logic_id, seen, op)
            # Unary `-` (Minus) has no distinct taxonomy glyph and stays
            # process-level (no gate), like Div/Mod's datapath deferral — unless
            # it folds to a constant, in which case surface the value (#199).
            return self._const_input(expr)
        # A bit concatenation / replication (#199) becomes a `Concat` primitive box
        # gathering its elements, so a mux data branch like `{x[31:2], 2'b00}` renders
        # with its input instead of vanishing.
        if "Concatenation" in k:
            operands = list(getattr(expr, "operands", None) or [])
            if operands:
                return self._emit_gate("Concat", expr, operands, logic_id, seen)
        if "Replication" in k:
            inner = getattr(expr, "concat", None)
            if inner is not None:
                return self._emit_gate("Concat", expr, [inner], logic_id, seen)
        return self._const_input(expr)

    def _fold_not(self, operand: Any) -> Optional[tuple[str, str]]:
        """If `operand` is an associative gate chain, the (folded kind, inner op)
        so `~(a & b)` becomes one Nand — else None (a plain inverter)."""
        if operand is None or "BinaryOp" not in _kind_name(operand):
            return None
        inner_op = _op_name(operand)
        folded = self._FOLD.get(self._BINOP_KIND.get(inner_op, ""))
        return (folded, inner_op) if folded is not None else None

    def _emit_gate(
        self,
        kind: str,
        expr: Any,
        operand_exprs: list[Any],
        logic_id: int,
        seen: set,
        op: Optional[str] = None,
    ) -> tuple[str, int]:
        nid = self._add_gate(kind, expr, logic_id, op)
        for oe in operand_exprs:
            self._wire_input(nid, self._emit_expr(oe, logic_id, seen), seen)
        return ("node", nid)

    def _emit_mux(self, expr: Any, logic_id: int, seen: set) -> tuple[str, int]:
        """A `?:` conditional -> a Mux: select on the south wall (`mux_port` sel),
        the true branch as D1 and the false branch as D0 (both west). Each input
        expression decomposes recursively."""
        nid = self._add_gate("Mux", expr, logic_id, "Conditional")
        conds = getattr(expr, "conditions", None) or []
        # A plain `sel ? a : b` has one condition. A pattern/`&&&`-joined ternary
        # (`c1 &&& c2 ? a : b`) is rare in RTL; we take the first condition as the
        # select and leave the rest process-level rather than guess a gate for them.
        sel = getattr(conds[0], "expr", None) if conds else None
        self._wire_input(nid, self._emit_expr(sel, logic_id, seen), seen, "sel")
        self._wire_input(
            nid, self._emit_expr(getattr(expr, "left", None), logic_id, seen), seen, "d1"
        )
        self._wire_input(
            nid, self._emit_expr(getattr(expr, "right", None), logic_id, seen), seen, "d0"
        )
        return ("node", nid)

    def _const_input(self, expr: Any) -> Optional[tuple[str, Any, Any]]:
        """A hard-coded literal operand (`8'hFF`, `'x`, #199) → a ``("const", lit,
        expr)`` tie, or None if the sub-expression isn't a bare literal/constant."""
        lit = self._const_str(expr)
        if lit is None:
            lit = self._literal_value(expr)
        return ("const", lit, expr) if lit is not None else None

    def _literal_value(self, expr: Any) -> Optional[str]:
        """The value of a bare integer literal (#199). slang leaves `.constant`
        unset for a literal in a non-constant context (e.g. an `'x` don't-care
        branch of a mux), but the `SVInt` is always on `.value` — so a `sel ? a :
        'x` else-branch still ties instead of vanishing."""
        k = _kind_name(expr)
        if "IntegerLiteral" not in k:  # covers Unbased/UnsizedIntegerLiteral too
            return None
        v = getattr(expr, "value", None)
        s = str(v) if v is not None else ""
        return s if s and s != "None" else None

    def _param_value(self, expr: Any) -> Optional[str]:
        """The resolved value of a parameter reference (#199), else None. A parameter
        `NamedValue` carries no `.constant`, but its symbol holds the elaborated
        `.value` (e.g. `8'd170`)."""
        sym = getattr(expr, "symbol", None)
        if sym is None or "Parameter" not in _kind_name(sym):
            return None
        val = getattr(sym, "value", None)
        s = str(val) if val is not None else ""
        return s if s and s != "None" else None

    def _add_const(self, lit: str, gate_id: int, expr: Any) -> int:
        """A synthetic `Const` node carrying a literal operand's value (#199), a flat
        child of the gate's logic block. The schematic turns it into a tie value on
        the gate input, rendered like an instance-port constant tie."""
        nid = len(self.nodes)
        parent = self.nodes[gate_id]["parent"]
        base = self.nodes[parent]["path"]
        path = f"{base}.$const{nid}"
        self.nodes.append(
            {
                "id": nid,
                "kind": "Const",
                "name": "const",
                "path": path,
                "parent": parent,
                "children": [],
                "symbol_key": path,
                "def_range": self._range_from_expr(expr),
                "inst_range": None,
                "type": None,
                "dir": None,
                "const": lit,
                "modport": None,
                "drivers": [],
                "loads": [],
            }
        )
        self.nodes[parent]["children"].append(nid)
        return nid

    def _wire_input(
        self,
        gate_id: int,
        endpoint: Optional[tuple[str, Any]],
        seen: set,
        mux_port: Optional[str] = None,
    ) -> None:
        """Wire a resolved endpoint into `gate_id` as an input. A ``("node", nid)``
        is another primitive (gate-to-gate); a ``("sig", path)`` resolves to the real
        scalar/bus signal it reads; ``("const", lit, expr)`` synthesizes a literal
        tie node; ``("param", path, lit)`` wires to a parameter's node and stamps its
        resolved value (#199)."""
        if endpoint is None:
            return
        tag = endpoint[0]
        if tag == "node":
            seen.add((gate_id, endpoint[1], "in", None, None, mux_port))
            return
        if tag == "const":
            cid = self._add_const(endpoint[1], gate_id, endpoint[2])
            seen.add((gate_id, cid, "in", None, None, mux_port))
            return
        if tag == "param":
            _, path, lit = endpoint
            sid = self._pick_node(path, ("Param", "Net", "Var", "Port"))
            if sid is not None and sid != gate_id:
                if self.nodes[sid]["kind"] == "Param":
                    self.nodes[sid]["const"] = lit
                seen.add((gate_id, sid, "in", None, None, mux_port))
            return
        val = endpoint[1]
        sid = self._pick_node(val, ("Net", "Var", "Port"))
        if (
            sid is not None
            and sid != gate_id
            and self.nodes[sid]["kind"] in ("Net", "Var", "Port")
        ):
            seen.add((gate_id, sid, "in", None, None, mux_port))

    def _gate_assignments(self, sym: Any) -> list[tuple[Any, Any]]:
        """Every ``(lhs, rhs)`` this block drives, for gate decomposition. `lhs` is
        an l-value expression, or ``("path", str)`` for a net-declaration
        initializer (whose target is not in the expression tree, #110)."""
        tp = getattr(sym, "target_path", None)
        if tp is not None:
            return [(("path", tp), getattr(sym, "assignment", None))]
        root = getattr(sym, "assignment", None) or sym
        out: list[tuple[Any, Any]] = []

        def cb(n: Any) -> None:
            if "Assignment" in _kind_name(n):
                out.append((getattr(n, "left", None), getattr(n, "right", None)))

        try:
            root.visit(cb)
        except Exception:
            pass
        return out

    def _gate_block(self, logic_id: int, sym: Any, role: str, seen: set) -> None:
        """Decompose one logic block's driven expressions into a gate/mux network
        feeding the block's assigned signals. Runs *in addition* to `_logic_edges`;
        the root primitive of each RHS wires out to that assignment's l-value."""
        for lhs, rhs in self._gate_assignments(sym):
            out = self._emit_expr(rhs, logic_id, seen)
            if out is None or out[0] != "node":
                # A bare signal / constant RHS (`assign y = x;`) has no gate — the
                # direct wire is already carried by the process-level edges.
                continue
            root_id = out[1]
            if isinstance(lhs, tuple) and lhs and lhs[0] == "path":
                targets = {lhs[1]}
            elif lhs is not None:
                targets = self._lvalue_bases(lhs)
            else:
                targets = set()
            for lp in sorted(targets):
                sid = self._pick_node(lp, ("Net", "Var", "Port"))
                if (
                    sid is not None
                    and sid != root_id
                    and self.nodes[sid]["kind"] in ("Net", "Var", "Port")
                ):
                    seen.add((root_id, sid, "out", None, None, None))

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

        # A generate branch the elaboration discarded is not part of the design, so it
        # is not part of the model (#178). Every branch of an unnamed if-generate
        # shares one LRM-implicit name (`genblk1`), so emitting the dead ones put
        # several nodes on one canonical path — the tree drew a row per branch, and
        # `find`-by-path answered with whichever came first. Worse, a dead branch's
        # members are real symbols: `picorv32`'s `if (TWO_CYCLE_ALU)` branch emitted its
        # `always @(posedge clk)` as an `Ff` that double-drove `alu_add_sub` alongside
        # the live `always @*` — logic no simulator would ever run.
        #
        # `isUninstantiated` is the only sound test. Not `name == ""`: a generate-for's
        # iteration blocks (`g_lane[0]`) are unnamed and very much live. Returning,
        # rather than just skipping the node, is what drops the dead members with it —
        # descending would reparent them onto the enclosing scope.
        if kind == "GenBlock" and getattr(sym, "isUninstantiated", False):
            return

        # A process / continuous assign becomes a logic spine node: an inferred
        # register (`ff`) or a combinational block (`comb`). Its read/assigned (and
        # clock) wiring is recovered later in `_logic_edges`.
        role = self._logic_role(sym)
        if role is not None:
            logic_id = self._add_logic(sym, parent, role)
            self.logic_blocks.append((logic_id, sym, role))
            return

        # `initial` stays non-logic (ADR 0004); collect it for the $readmemh
        # INIT-marker pass, then keep descending as before (#112).
        if self._is_initial(sym):
            self.init_blocks.append(sym)

        # Skip the auto-generated internal variable backing a port (the Port node
        # already represents that signal), to avoid duplicate same-path nodes.
        if kind == "Var" and getattr(sym, "isCompilerGenerated", False):
            return

        # A variable with an unpacked dimension (`logic [W-1:0] ram [0:N-1]`) is a
        # memory array — retag so the drilled logic view draws a MEMORY glyph
        # instead of a wire. Process-granularity per ADR 0004: the box maps to the
        # array's own `def_range`, so cross-probe stays a lookup (#112).
        mem_depth: Optional[int] = None
        if kind == "Var":
            mem_depth = self._memory_depth(sym)
            if mem_depth is not None:
                kind = "Memory"

        my_id = parent
        if kind is not None:
            # An interface instance is a slang `Instance` whose definition is an
            # interface; retag it so the schematic can draw a signal bundle.
            if kind == "Instance" and _is_interface_instance(sym):
                kind = "Interface"
            my_id = self._add(sym, kind, parent)
            if kind == "Memory" and mem_depth is not None:
                self.nodes[my_id]["mem_depth"] = mem_depth
            # Both module and interface instances carry port connections (an
            # interface has its own ports, e.g. `.clk`), so collect both for edge
            # extraction. The slang symbol kind is `Instance` for each.
            if kname == "Instance":
                self.instances.append((my_id, sym))
            # A modport-specialized interface port pins its members (#64).
            if kname == "InterfacePort" and self.nodes[my_id]["modport"]:
                self._add_modport_pins(sym, my_id)
            # `wire w = expr;` — a net declaration initializer is a continuous
            # assign the member walk otherwise misses (#110): emit an Assign
            # node so the cone through `w` stays traceable. A *variable*
            # initializer runs once at time zero (like `initial`) and stays
            # non-logic. Passing the net symbol anchors `def_range` at the
            # declaration for source cross-probe.
            if kind == "Net":
                init = getattr(sym, "initializer", None)
                if init is not None:
                    aid = self._add_logic(sym, parent, "assign")
                    self.logic_blocks.append(
                        (aid, _NetInitializer(init, self.nodes[my_id]["path"]), "assign")
                    )

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
        path = _value_sym_path(getattr(expr, "symbol", None))
        if path:
            out.append(path)
        getref = getattr(expr, "getSymbolReference", None)
        if callable(getref):
            try:
                path = _value_sym_path(getref())
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
        """Resolve a path to a node id, preferring the given kinds in order.

        Modport pins share their member's path but are views, never the member
        itself — skip them whenever anything else exists at the path, so logic
        and connection endpoints land on the real signal regardless of the
        declaration order of consumer and interface instance."""
        ids = self._by_path.get(path)
        if not ids:
            return None
        real = [t for t in ids if t[0] not in self._modport_pin_ids] or ids
        for want in prefer:
            for nid, kind in real:
                if kind == want:
                    return nid
        return real[0][0]

    def _edges(self) -> list[dict[str, Any]]:
        # path -> [(id, kind)] for resolving connection endpoints.
        self._by_path: dict[str, list[tuple[int, str]]] = {}
        for n in self.nodes:
            self._by_path.setdefault(n["path"], []).append((n["id"], n["kind"]))

        dir_map = {"In": "in", "Out": "out", "InOut": "inout"}
        # Collect as a set keyed by (port, endpoint, dir, select, mem_port,
        # mux_port), then sort, so the output is canonical regardless of pyslang
        # container iteration order. `select` is the resolved bit-select (e.g.
        # `[0]`) or None for the whole signal; `mem_port` tags a memory-array pin
        # ("addr"/"din"/"dout", #112); `mux_port` tags a gate-level mux input
        # ("sel"/"d0"/"d1", #157). All but the first three are None on an ordinary
        # edge.
        seen_edges: set[
            tuple[int, int, str, Optional[str], Optional[str], Optional[str]]
        ] = set()
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
                        seen_edges.add(
                            (port_id, end_id, direction, selects.get(rp), None, None)
                        )
                # Input tied to a literal (no net): record the constant on the
                # port so the schematic can show its driver (e.g. 32'd0).
                if not refs and port_id != inst_id:
                    lit = self._const_str(expr)
                    if lit is not None and self.nodes[port_id]["kind"] == "Port":
                        self.nodes[port_id]["const"] = lit

        # A modport member pin connects to its underlying bundle member — the
        # same shape as a module port connection, so the schematic can wire and
        # filter interface pins uniformly with instance pins.
        for pid, mpath, d in self.modport_pins:
            end_id = self._pick_node(mpath, ("Net", "Var", "Port"))
            if end_id is not None and end_id != pid:
                seen_edges.add((pid, end_id, d, None, None, None))

        for logic_id, logic_sym, role in self.logic_blocks:
            self._logic_edges(logic_id, logic_sym, role, seen_edges)

        # Optional gate-level projection (#157): after the process-level edges are
        # wired, decompose each block's expressions into gate/mux nodes + edges.
        # Additive — the process nodes/edges above are untouched, so the schematic
        # can render either projection over the same spine.
        if self.gate_level:
            for logic_id, logic_sym, role in self.logic_blocks:
                self._gate_block(logic_id, logic_sym, role, seen_edges)

        edges: list[dict[str, Any]] = []
        ordered = sorted(
            seen_edges,
            key=lambda t: (t[0], t[1], t[2], t[3] or "", t[4] or "", t[5] or ""),
        )
        for i, (p, e, d, s, mp, xp) in enumerate(ordered):
            edge: dict[str, Any] = {"id": i, "port": p, "endpoint": e, "dir": d}
            if s is not None:
                edge["select"] = s
            if mp is not None:
                edge["mem_port"] = mp
            if xp is not None:
                edge["mux_port"] = xp
            edges.append(edge)
        return edges

    def _apply_inits(self) -> None:
        """Attribute each `initial $readmemh/$readmemb(FILE, ram)` to its target
        memory as `init_source` metadata (the INIT marker). Runs after `_edges`
        so `_by_path` resolves the memory node. `initial` is never a logic node
        (ADR 0004) — this only annotates the array (#112)."""
        for ib in self.init_blocks:
            calls: list[Any] = []

            def collect(n: Any) -> None:
                if "Call" in _kind_name(n) and getattr(n, "isSystemCall", False):
                    if getattr(n, "subroutineName", "") in ("$readmemh", "$readmemb"):
                        calls.append(n)

            try:
                ib.visit(collect)
            except Exception:
                continue
            for call in calls:
                args = list(getattr(call, "arguments", None) or [])
                if len(args) < 2:
                    continue
                # slang models the memory (output) arg as an Assignment to it.
                mem_arg = args[1]
                left = getattr(mem_arg, "left", mem_arg)
                targets = self._lvalue_bases(left)
                # The source-file arg: a parameter/net name, else a string literal.
                src_sym = getattr(args[0], "symbol", None)
                source = getattr(src_sym, "name", None)
                if not source:
                    c = getattr(args[0], "constant", None)
                    source = str(c) if c is not None and not getattr(c, "bad", False) else "$readmem"
                for tp in targets:
                    nid = self._pick_node(tp, ("Memory", "Var"))
                    if nid is not None and self.nodes[nid]["kind"] == "Memory":
                        self.nodes[nid]["init_source"] = source

    def build(self) -> dict[str, Any]:
        root = self.comp.getRoot()
        # Slang's analysis pass flags `always_comb` blocks that infer a latch; used
        # by `_logic_role` to classify them `Latch` rather than `Comb`.
        self._inferred_latch_locs = self._collect_inferred_latches()
        for top in root.topInstances:
            self._walk(top, None)
        edges = self._edges()
        self._apply_inits()
        return {
            "schema_version": SCHEMA_VERSION,
            "design": root.topInstances[0].name if root.topInstances else "",
            "generator": {"tool": "pyslang", "version": pyslang.__version__},
            "files": self.files,
            "nodes": self.nodes,
            "edges": edges,
            "enums": self.enums,
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
    gate_level: bool = False,
) -> dict[str, Any]:
    return Elaborator(files, top, include_dirs, gate_level=gate_level).build()


def _error_report(el: Elaborator) -> Optional[str]:
    """Rendered compile *errors* from the elaboration, or None when clean.
    slang reports these as diagnostics, never as an exception — without this a
    caller (the app's designlist flow, CI) gets an empty/partial model with no
    explanation."""
    errors = [d for d in el.comp.getAllDiagnostics() if d.isError()]
    if not errors:
        return None
    try:
        return str(pyslang.DiagnosticEngine.reportAll(el.sm, errors))
    except Exception:
        return f"{len(errors)} compile error(s) (diagnostic rendering failed)"


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
    ap.add_argument(
        "--gate-level",
        action="store_true",
        help="Also emit the optional gate-level projection: decompose process/"
        "assign expressions into gate/mux primitive nodes (#157). Off by default; "
        "the default output is unchanged.",
    )
    args = ap.parse_args(argv)

    files: list[str] = list(args.files)
    include_dirs: list[str] = list(args.include)
    seen: set[str] = set()
    for fl in args.filelist:
        parse_filelist(fl, files, include_dirs, seen)

    if not files:
        ap.error("no source files given (pass files and/or -f FILELIST)")

    el = Elaborator(files, args.top, include_dirs, gate_level=args.gate_level)
    model = el.build()
    # Compile errors always render to stderr for visibility, but only an empty
    # design (nothing elaborated) fails the run: slang flags pedantic errors
    # (e.g. mixed timescale presence) on designs it still elaborates fully, and
    # the harness stays best-effort about partial models.
    report = _error_report(el)
    if report is not None:
        print(report, file=sys.stderr, end="")
    if not model["design"]:
        where = f" for top '{args.top}'" if args.top else ""
        hint = " (see diagnostics above)" if report is not None else ""
        print(f"error: elaboration produced no design{where}{hint}", file=sys.stderr)
        return 1
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
