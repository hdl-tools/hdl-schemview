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
            if kind in ("Net", "Port", "Var"):
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
            # An interface instance is a slang `Instance` whose definition is an
            # interface; retag it so the schematic can draw a signal bundle.
            if kind == "Instance" and _is_interface_instance(sym):
                kind = "Interface"
            my_id = self._add(sym, kind, parent)
            # Both module and interface instances carry port connections (an
            # interface has its own ports, e.g. `.clk`), so collect both for edge
            # extraction. The slang symbol kind is `Instance` for each.
            if kname == "Instance":
                self.instances.append((my_id, sym))
            # A modport-specialized interface port pins its members (#64).
            if kname == "InterfacePort" and self.nodes[my_id]["modport"]:
                self._add_modport_pins(sym, my_id)

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

        # A modport member pin connects to its underlying bundle member — the
        # same shape as a module port connection, so the schematic can wire and
        # filter interface pins uniformly with instance pins.
        for pid, mpath, d in self.modport_pins:
            end_id = self._pick_node(mpath, ("Net", "Var", "Port"))
            if end_id is not None and end_id != pid:
                seen_edges.add((pid, end_id, d, None))

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
        # Slang's analysis pass flags `always_comb` blocks that infer a latch; used
        # by `_logic_role` to classify them `Latch` rather than `Comb`.
        self._inferred_latch_locs = self._collect_inferred_latches()
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
) -> dict[str, Any]:
    return Elaborator(files, top, include_dirs).build()


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
    args = ap.parse_args(argv)

    files: list[str] = list(args.files)
    include_dirs: list[str] = list(args.include)
    seen: set[str] = set()
    for fl in args.filelist:
        parse_filelist(fl, files, include_dirs, seen)

    if not files:
        ap.error("no source files given (pass files and/or -f FILELIST)")

    el = Elaborator(files, args.top, include_dirs)
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
