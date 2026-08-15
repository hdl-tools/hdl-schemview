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

# Default HLS provenance-comment pattern (#159). HLS tools (Vitis, Intel, Bambu,
# LegUp, ...) write the originating C/C++ file:line into the generated RTL as a
# line comment, e.g. `assign x = a + b;  // foo.cpp:42` or `// Operation 5
# [foo.cpp:42]`. This finds a `<path>.<ext>:<line>` reference anywhere after a `//`.
# Override with --hls-comment-re for a vendor-specific form.
HLS_COMMENT_RE = r"//.*?(?P<file>[\w./\\-]+\.(?:c|cc|cpp|cxx|h|hpp)):(?P<line>\d+)"

# C/C++ source extensions → the `language` tag stored on their file-table entry.
_C_LANGS = {
    ".c": "c",
    ".h": "c",
    ".cc": "cpp",
    ".cpp": "cpp",
    ".cxx": "cpp",
    ".hpp": "cpp",
}


def _lang_of(path: str) -> str:
    """The `language` tag for a C/C++ source path (defaults to cpp)."""
    _, ext = os.path.splitext(path)
    return _C_LANGS.get(ext.lower(), "cpp")


def _line_starts(data: bytes) -> list[int]:
    """Byte offset of the start of each line (0-based line index → offset). LF-based,
    matching the golden's offset basis (RTL/goldens are pinned LF via .gitattributes)."""
    starts = [0]
    for i, b in enumerate(data):
        if b == 0x0A:  # '\n'
            starts.append(i + 1)
    return starts

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


# Node-model kind -> NameClass for a *declaration* name token (#225). Only real
# declarations are listed: the logic spine (`Ff`/`Comb`/`Assign`) and the gate-level
# primitives declare nothing, so they never contribute a name ref.
_NAME_CLASS_BY_NODE_KIND = {
    "Instance": "instance",
    "Interface": "instance",
    "Modport": "modport",
    "Port": "port",
    "Net": "signal",
    "Var": "signal",
    "Memory": "signal",
    "Param": "param",
}

# slang SymbolKind name -> NameClass for a *reference* occurrence (#225). The class is
# read off the symbol slang resolved the identifier to — a lookup, never the token text.
# A symbol kind absent from this map yields no ref: an uncolored identifier, never a
# guessed one.
_NAME_CLASS_BY_SYM_KIND = {
    "Variable": "signal",
    "Net": "signal",
    "Port": "port",
    "InterfacePort": "port",
    "ModportPort": "port",
    "FormalArgument": "signal",
    "Parameter": "param",
    "TypeParameter": "param",
    "Specparam": "param",
    "EnumValue": "enum-member",
    "Genvar": "genvar",
    "Instance": "instance",
    "Subroutine": "function",
    "Modport": "modport",
}

# Tie-break when two records land on the same span with the same `rel` — the
# port/backing-net dual declaration is the real case (both sit on one token).
# Lower wins.
_NAME_CLASS_RANK = {
    "port": 0,
    "instance": 1,
    "modport": 2,
    "param": 3,
    "enum-member": 4,
    "genvar": 5,
    "function": 6,
    "type": 7,
    "signal": 8,
    "module": 9,
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
        hls_map: bool = False,
        hls_comment_re: Optional[str] = None,
        hls_src: Optional[list[str]] = None,
        name_refs: bool = False,
    ) -> None:
        # Opt-in identifier-occurrence spans (#225): every declaration name token and
        # every resolved value reference, so the source pane can color identifiers and
        # a click on a *usage* can resolve to the signal it names. Off by default ⇒
        # output byte-identical (no `name_refs` key at all).
        self.name_refs = name_refs
        self.name_ref_rows: list[dict[str, Any]] = []
        # (node id, declaring symbol) for every spine node, so the name-ref pass can
        # take each declaration's name token without re-walking the hierarchy.
        self._decl_syms: list[tuple[int, Any]] = []
        # (file, offset) -> the winning record for that span (see `_add_name_ref`).
        self._name_ref_index: dict[tuple[int, int], dict[str, Any]] = {}
        # Opt-in gate-level projection (#157, ADR 0005): decompose process/assign
        # RHS expressions into gate/mux primitive nodes *in addition* to the
        # process-level logic nodes. Off by default ⇒ output byte-identical.
        self.gate_level = gate_level
        # Opt-in HLS C/C++ ↔ RTL provenance map (#159): scan the generated RTL's
        # line-annotated comments for the originating C source and emit a source_map.
        # Off by default ⇒ output byte-identical (no `language`, no `source_map`).
        self.hls_map = hls_map
        self.hls_re = re.compile(hls_comment_re or HLS_COMMENT_RE)
        # Explicitly declared C/C++ sources (#222). A *file* is a declared source,
        # registered eagerly so a header no comment mentions is still browsable; a
        # *directory* is a search root for resolving provenance-comment paths (not
        # auto-registered — a src/ tree would pull in files nobody asked for). C is
        # never parsed (ADR 0006), so these are search paths, not compiler includes.
        self.hls_src_files: list[str] = []
        self.hls_src_roots: list[str] = []
        for _p in hls_src or []:
            (self.hls_src_roots if os.path.isdir(_p) else self.hls_src_files).append(_p)
        # Emitted C↔RTL correspondences and a cache of each scanned file's line-starts.
        self.source_map: list[dict[str, Any]] = []
        self._line_start_cache: dict[str, Optional[list[int]]] = {}
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
    def _file_id(self, path: str, language: Optional[str] = None) -> int:
        # Store paths with forward slashes so the golden is byte-identical across
        # OSes (slang yields the platform separator); a no-op on POSIX.
        path = path.replace("\\", "/")
        fid = self._file_ids.get(path)
        if fid is None:
            fid = len(self.files)
            self._file_ids[path] = fid
            entry: dict[str, Any] = {"id": fid, "path": path}
            # `language` is only ever set by the opt-in HLS pass (#159); the default
            # elaboration path never passes it, so default output stays byte-identical.
            if language is not None:
                entry["language"] = language
            self.files.append(entry)
        elif language is not None:
            self.files[fid].setdefault("language", language)
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

    # -- HLS provenance (#159) ----------------------------------------------
    def _read_line_starts(self, disk_path: str) -> Optional[list[int]]:
        """Line-start offsets for a file on disk, cached; None if unreadable."""
        if disk_path not in self._line_start_cache:
            try:
                with open(disk_path, "rb") as fh:
                    self._line_start_cache[disk_path] = _line_starts(fh.read())
            except OSError:
                self._line_start_cache[disk_path] = None
        return self._line_start_cache[disk_path]

    @staticmethod
    def _line_range(file_id: int, starts: list[int], line_1based: int) -> Optional[dict[str, Any]]:
        """A whole-line source range (col 1..end) for a 1-based line number."""
        idx = line_1based - 1
        if idx < 0 or idx >= len(starts):
            return None
        start_off = starts[idx]
        end_off = starts[idx + 1] if idx + 1 < len(starts) else start_off
        return {
            "file": file_id,
            "start": {"line": line_1based, "col": 1, "offset": start_off},
            "end": {"line": line_1based, "col": 1, "offset": end_off},
        }

    def _scan_hls_provenance(self) -> None:
        """Scan the generated RTL for HLS provenance comments and build the source_map.

        Line-based over each already-registered RTL file's raw text (independent of
        pyslang trivia). Each `<c-file>:<line>` comment links that RTL line to the C
        line — the authoritative C↔RTL correspondence, a lookup not a heuristic. Tags
        RTL files `systemverilog` and each referenced C file by extension.
        """
        rtl_files = list(self.files)  # snapshot: only RTL entries exist pre-scan
        for entry in rtl_files:
            entry["language"] = "systemverilog"
            rtl_path = entry["path"]
            data: Optional[bytes]
            try:
                with open(rtl_path, "rb") as fh:
                    data = fh.read()
            except OSError:
                continue  # best-effort: an unreadable source just yields no map
            starts = _line_starts(data)
            self._line_start_cache[rtl_path] = starts
            rtl_dir = os.path.dirname(rtl_path)
            for line_idx, start in enumerate(starts):
                end = starts[line_idx + 1] if line_idx + 1 < len(starts) else len(data)
                text = data[start:end].decode("utf-8", "replace")
                m = self.hls_re.search(text)
                if not m:
                    continue
                c_ref, c_line = m.group("file"), int(m.group("line"))
                c_disk = self._resolve_c_ref(c_ref, rtl_dir)
                c_starts = self._read_line_starts(c_disk)
                if c_starts is None:
                    continue  # C source not on disk → can't build a real range
                gen = self._line_range(entry["id"], starts, line_idx + 1)
                c_id = self._file_id(c_disk, language=_lang_of(c_ref))
                src = self._line_range(c_id, c_starts, c_line)
                if gen is not None and src is not None:
                    self.source_map.append({"generated": gen, "source": src})

    def _resolve_c_ref(self, c_ref: str, rtl_dir: str) -> str:
        """Resolve a provenance comment's C path to a path on disk (#222).

        Ordered, deterministic and declaration-driven — never a name guess:

        1. an absolute ref that exists on disk;
        2. each declared search root, in declaration order;
        3. a declared source whose basename matches — only when *exactly one* does
           (several ⇒ genuinely ambiguous, so warn and fall through rather than
           silently pick one);
        4. the RTL file's own directory — the pre-#222 behavior, so a design that
           declares nothing resolves exactly as it did before.

        Rung 1 falling through when the path does not exist is what rescues a
        vendor-mangled absolute path baked in on the build machine.
        """
        if os.path.isabs(c_ref):
            if os.path.exists(c_ref):
                return c_ref
        else:
            for root in self.hls_src_roots:
                cand = os.path.normpath(os.path.join(root, c_ref))
                if os.path.exists(cand):
                    return cand
        base = os.path.basename(c_ref)
        matches = [p for p in self.hls_src_files if os.path.basename(p) == base]
        if len(matches) == 1:
            return matches[0]
        if len(matches) > 1:
            print(
                f"svxprobe-elaborate: ambiguous C source basename {base!r} "
                f"({len(matches)} declared sources match); not resolving by basename",
                file=sys.stderr,
            )
        # Keep the original basis so `src_root` resolves RTL and C the same way.
        if os.path.isabs(c_ref):
            return c_ref
        return os.path.normpath(os.path.join(rtl_dir, c_ref))

    def _register_declared_c_sources(self) -> None:
        """Register every explicitly declared C/C++ source file (#222).

        The files table is otherwise populated purely as a side effect of range
        registration, so a header that no provenance comment mentions never appears
        at all. Registering it here makes it browsable in the C pane even when it has
        no RTL correspondence. Runs *after* `_scan_hls_provenance`, whose RTL snapshot
        would otherwise tag these files `systemverilog`.
        """
        for path in self.hls_src_files:
            self._file_id(path, language=_lang_of(path))

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
        self._decl_syms.append((nid, sym))
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
        # The right-hand side of each assignment — what the block *states*, kept
        # alongside what it assigns so a block stating only a literal can record
        # it (#298).
        stated: list[Any] = []

        def cb(n: Any) -> None:
            if "Assignment" in _kind_name(n):
                left = getattr(n, "left", None)
                if left is not None:
                    assigned.update(self._lvalue_bases(left))
                stated.append(getattr(n, "right", None))

        try:
            root.visit(cb)
        except Exception:
            pass
        # A net declaration initializer's target is not in the expression tree
        # (there is no Assignment node) — the adapter names it directly, and its
        # whole expression is the assigned value.
        target = getattr(sym, "target_path", None)
        if target is not None:
            assigned.add(target)
            stated.append(root)
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
        # A block that states a literal rather than computing one records that
        # literal, so the schematic can label the net it drives `pcpi_mul_wr =
        # 1'b0` (#298) instead of leaving a tied net indistinguishable from a
        # live one. Both guards are the claim itself: exactly one assignment
        # (several would be several values, and the net-level statement no
        # longer has a single subject) and no data reads (a block that reads a
        # signal computes a value rather than stating one). Sequential roles are
        # excluded — an FF's output is its *state*, not its RHS.
        if role in ("assign", "comb") and not data and len(stated) == 1:
            rhs = stated[0]
            tie = self._const_input(rhs) if rhs is not None else None
            if tie is not None:
                self.nodes[logic_id]["const"] = tie[1]
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
        # `Memory` is accepted alongside scalar signals (#206): a gate/mux operand that
        # reads an array element (`cpuregs[decoded_rs1]`) resolves via `_leaf_signal` to
        # the whole array node, so the input wires to the memory — the index being the
        # same fidelity simplification as a peeled bit-select. Without it the branch was
        # dropped, leaving those muxes one input short.
        sid = self._pick_node(val, ("Net", "Var", "Port", "Memory"))
        if (
            sid is not None
            and sid != gate_id
            and self.nodes[sid]["kind"] in ("Net", "Var", "Port", "Memory")
        ):
            seen.add((gate_id, sid, "in", None, None, mux_port))

    def _block_assignments(self, sym: Any) -> list[tuple[Any, Any, Any]]:
        """Every ``(node, lhs, rhs)`` this block drives, for gate decomposition.
        `node` is the pyslang Assignment expression (``None`` for a net-declaration
        initializer, #110), used to exclude assignments a `case` pre-pass already
        lowered (see `_lower_cases`); `lhs` is an l-value expression, or ``("path",
        str)`` for the net-init target (which is not in the expression tree)."""
        tp = getattr(sym, "target_path", None)
        if tp is not None:
            return [(None, ("path", tp), getattr(sym, "assignment", None))]
        root = getattr(sym, "assignment", None) or sym
        out: list[tuple[Any, Any, Any]] = []

        def cb(n: Any) -> None:
            if "Assignment" in _kind_name(n):
                out.append((n, getattr(n, "left", None), getattr(n, "right", None)))

        try:
            root.visit(cb)
        except Exception:
            pass
        return out

    def _gate_block(self, logic_id: int, sym: Any, role: str, seen: set) -> None:
        """Decompose one logic block's driven expressions into a gate/mux network
        feeding the block's assigned signals. Runs *in addition* to `_logic_edges`;
        the root primitive of each RHS wires out to that assignment's l-value.

        A `case`/`casez`/`casex` (#207) or `if`/`else` (#215) statement is first
        lowered structurally into a mux chain (`_lower_stmts`); the branch
        assignments it consumes are then skipped by the flat pass below so they are
        not double-wired."""
        consumed: set = set()
        body = getattr(sym, "body", None)
        if body is not None:
            self._lower_stmts(body, logic_id, seen, consumed)
        for node, lhs, rhs in self._block_assignments(sym):
            key = self._assign_key(node)
            if key is not None and key in consumed:
                continue
            out = self._emit_expr(rhs, logic_id, seen)
            if out is None or out[0] != "node":
                # A bare signal / constant RHS (`assign y = x;`) has no gate — the
                # direct wire is already carried by the process-level edges.
                continue
            self._wire_out(out[1], lhs, seen)

    def _wire_out(self, root_id: int, lhs: Any, seen: set) -> None:
        """Wire a root primitive `root_id` out to the l-value `lhs` (an expression,
        or ``("path", str)`` for a net-init target)."""
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

    # -- statement -> mux-tree lowering (#207 `case`, #215 `if`/`else`) --------
    #
    # The flat `_block_assignments` visit throws away the enclosing statement
    # structure, so a branch like `alu_out = alu_add_sub;` arrives as a bare-signal
    # RHS and emits no gate — leaving the read signal's producer readerless, and an
    # `if (c) y = a; else y = b;` arrives as two independent writes to `y` with the
    # condition `c` dropped entirely. This pass walks the block's statements in
    # source order, tracks each l-value's most-recent value (the fall-through
    # default), and rewrites every `case` (#207) and every `if`/`else` (#215) into a
    # right-leaning Mux chain (ADR 0005 §1; reuses the existing `Mux` kind +
    # `mux_port` sel/d0/d1 roles, so no schema change).
    #
    # An l-value's pending value is one of:
    #   ("assign", node, rhs)  — a source assignment not yet emitted, so it can be
    #                            marked `consumed` only if the lowering actually
    #                            uses it (see `_resolve_pending`);
    #   an endpoint tuple      — ("node", nid) / ("sig", path) / ("const", ...),
    #                            already emitted by a nested lowering.
    # The wire-out to the real signal happens once, at the end of `_lower_stmts`,
    # from each l-value's *final* value — so a `case` nested inside an `if` feeds
    # the outer Mux instead of driving the signal a second time.

    def _flatten_stmts(self, stmt: Any) -> list[Any]:
        """The statement sequence in source order, peeling the structural wrappers
        (`Timed` timing controls, `Block` begin/end, `List`) — mirrors the peeling in
        `_gating_condition_refs`. Does *not* descend into `if`/`case` branches; the
        lowering recurses into those itself (`_lower_branch`)."""
        out: list[Any] = []

        def rec(s: Any) -> None:
            if s is None:
                return
            k = self._stmt_kind(s)
            if k == "Timed":
                rec(getattr(s, "stmt", None))
            elif k == "Block":
                rec(getattr(s, "body", None))
            elif k == "List":
                for it in getattr(s, "list", None) or []:
                    rec(it)
            else:
                out.append(s)

        rec(stmt)
        return out

    def _assign_key(self, assign: Any) -> Optional[tuple]:
        """A stable identity for an assignment expression — its source-range byte
        offsets — so a `case` pre-pass can tell the flat pass which branch
        assignments it already lowered. Keyed on source (not ``id()``): pyslang
        re-wraps a node on each traversal, so wrapper ``id()`` is unstable across the
        two passes (its reuse is even hash-seed dependent). ``consumed`` is per-block,
        so identical offsets across replicated scopes (e.g. `g_lane[0/1]`) never
        cross-contaminate."""
        if assign is None:
            return None
        try:
            sr = assign.sourceRange
            return (sr.start.offset, sr.end.offset)
        except Exception:
            return None

    def _mark_consumed(self, assign: Any, consumed: set) -> None:
        """Record that a `case` branch/default assignment was lowered, so the flat
        pass skips it (see `_assign_key`)."""
        key = self._assign_key(assign)
        if key is not None:
            consumed.add(key)

    def _resolve_pending(
        self, pending: Optional[tuple], logic_id: int, seen: set, consumed: set
    ) -> Optional[tuple]:
        """Turn an l-value's pending value into a wireable endpoint. An
        ``("assign", node, rhs)`` is emitted now and its source assignment marked
        `consumed` — only here, so an assignment the lowering never actually uses
        still reaches the flat pass. An already-emitted endpoint passes through."""
        if pending is None:
            return None
        if pending[0] == "assign":
            _, node, rhs = pending
            self._mark_consumed(node, consumed)
            return self._emit_expr(rhs, logic_id, seen) if rhs is not None else None
        return pending

    def _lower_stmts(
        self, body: Any, logic_id: int, seen: set, consumed: set
    ) -> None:
        """Lower `body`'s `case` (#207) and `if`/`else` (#215) statements into Mux
        trees, then wire each l-value's final lowered root out to its signal."""
        # lhs path -> pending value of the most-recent write, used as the next
        # structure's fall-through default (e.g. picorv32's `alu_out = 'bx;`).
        values: dict[str, tuple] = {}
        self._lower_seq(body, logic_id, seen, values, consumed)
        # Wire out once, from the final value — sorted for deterministic output.
        for lp in sorted(values):
            v = values[lp]
            if v is not None and v[0] == "node":
                self._wire_out(v[1], ("path", lp), seen)

    def _lower_seq(
        self, body: Any, logic_id: int, seen: set, values: dict, consumed: set
    ) -> None:
        """Fold one statement sequence into `values` in source order: a plain
        assignment becomes that l-value's pending value, a `case`/`if` rewrites its
        l-values into Mux chains over whatever they held coming in."""
        for s in self._flatten_stmts(body):
            k = self._stmt_kind(s)
            if k == "ExpressionStatement":
                assign = getattr(s, "expr", None)
                if assign is not None and "Assignment" in _kind_name(assign):
                    rhs = getattr(assign, "right", None)
                    for lp in self._lvalue_bases(getattr(assign, "left", None)):
                        values[lp] = ("assign", assign, rhs)
            elif k == "Case":
                self._emit_case_stmt(s, logic_id, seen, values, consumed)
            elif k == "Conditional":
                self._emit_cond_stmt(s, logic_id, seen, values, consumed)

    # -- if/else -> mux tree (#215) -------------------------------------------

    def _cond_chain(self, stmt: Any) -> tuple[list[tuple[Any, Any]], Any]:
        """Flatten an `if` / `else if` cascade into ``([(cond, body), ...], else)``.
        slang nests an `else if` as the parent's `ifFalse`, so the chain is walked
        rather than recursed — giving the same priority-ordered branch list
        `_emit_case_stmt` builds, and a final `else` body (None if absent)."""
        branches: list[tuple[Any, Any]] = []
        s = stmt
        while s is not None and self._stmt_kind(s) == "Conditional":
            conds = getattr(s, "conditions", None) or []
            # A `&&&`-joined / pattern condition is rare in RTL; take the first as
            # the select rather than guess a gate for the rest (mirrors `_emit_mux`).
            cexpr = getattr(conds[0], "expr", None) if conds else None
            branches.append((cexpr, getattr(s, "ifTrue", None)))
            s = getattr(s, "ifFalse", None)
        return branches, s

    def _assigned_lvalues(self, bodies: list[Any]) -> list[str]:
        """Every l-value path assigned anywhere under `bodies`, in first-seen order.
        Descends through nested `if`/`case` so one pass over the construct knows all
        the signals it writes — each is then given its own Mux chain."""
        order: list[str] = []
        found: set[str] = set()

        def rec(stmt: Any) -> None:
            for s in self._flatten_stmts(stmt):
                k = self._stmt_kind(s)
                if k == "ExpressionStatement":
                    assign = getattr(s, "expr", None)
                    if assign is not None and "Assignment" in _kind_name(assign):
                        for lp in sorted(
                            self._lvalue_bases(getattr(assign, "left", None))
                        ):
                            if lp not in found:
                                found.add(lp)
                                order.append(lp)
                elif k == "Case":
                    for grp in getattr(s, "items", None) or []:
                        rec(getattr(grp, "stmt", None))
                    rec(getattr(s, "defaultCase", None))
                elif k == "Conditional":
                    rec(getattr(s, "ifTrue", None))
                    rec(getattr(s, "ifFalse", None))

        for b in bodies:
            rec(b)
        return order

    def _lower_branch(
        self,
        body: Any,
        seeds: dict,
        logic_id: int,
        seen: set,
        consumed: set,
    ) -> dict:
        """The endpoint each l-value holds after one branch body runs. Seeded with
        `seeds` so a branch that does *not* write an l-value falls through to the
        value it had coming in (rather than to the `else`'s), and so a nested `if`
        with no `else` inherits the enclosing default. Recurses through nested
        `if`/`case` via `_lower_seq`, which is how a `case` inside an `if` — never
        reachable under #207's top-level-only pre-pass — gets lowered at all."""
        local = dict(seeds)
        if body is not None:
            self._lower_seq(body, logic_id, seen, local, consumed)
        return {
            lp: self._resolve_pending(local.get(lp), logic_id, seen, consumed)
            for lp in local
        }

    def _emit_cond_stmt(
        self, stmt: Any, logic_id: int, seen: set, values: dict, consumed: set
    ) -> None:
        """Lower one `if`/`else if`/`else` into a right-leaning Mux chain per
        assigned l-value: the branch condition is the select, the branch's value D1,
        and the rest of the chain (next branch, then the `else` or the fall-through
        prior write) D0 — the statement form of `_emit_mux`."""
        branches, else_body = self._cond_chain(stmt)
        order = self._assigned_lvalues(
            [b for _, b in branches] + ([else_body] if else_body is not None else [])
        )
        if not order:
            return
        # Each l-value's incoming value, resolved once and shared by every branch
        # that does not overwrite it (so the default's gates are emitted once).
        seeds = {
            lp: self._resolve_pending(values.pop(lp, None), logic_id, seen, consumed)
            for lp in order
        }
        # An absent `else` leaves every l-value at its incoming value — the same
        # fall-through model a `case` without `default:` uses, so a combinational
        # `if` does not read as a latch in the view.
        else_vals = (
            self._lower_branch(else_body, seeds, logic_id, seen, consumed)
            if else_body is not None
            else dict(seeds)
        )
        branch_vals = [
            (cond, self._lower_branch(body, seeds, logic_id, seen, consumed))
            for cond, body in branches
        ]
        for lp in order:
            acc = else_vals.get(lp)
            for cond, vals in reversed(branch_vals):
                nid = self._add_gate("Mux", cond, logic_id, "Conditional")
                self._wire_input(
                    nid, self._emit_expr(cond, logic_id, seen), seen, "sel"
                )
                self._wire_input(nid, vals.get(lp), seen, "d1")
                self._wire_input(nid, acc, seen, "d0")
                acc = ("node", nid)
            if acc is not None and acc[0] == "node":
                values[lp] = acc

    def _emit_case_stmt(
        self, case: Any, logic_id: int, seen: set, values: dict, consumed: set
    ) -> None:
        """Lower one `case` statement: build a priority-mux chain per assigned
        l-value and record its root as that l-value's new value (`_lower_stmts`
        wires the final one out, so a nested `case` feeds its enclosing Mux
        instead of driving the signal twice)."""
        case_expr = getattr(case, "expr", None)
        const_idiom = self._is_case_true(case_expr)
        # Per-l-value branches in source (priority) order: [(labels, rhs), ...].
        per_lhs: dict[str, list[tuple[list, Any]]] = {}
        order: list[str] = []
        for grp in getattr(case, "items", None) or []:
            labels = list(getattr(grp, "expressions", None) or [])
            st = getattr(grp, "stmt", None)
            assign = getattr(st, "expr", None) if st is not None else None
            if assign is None or "Assignment" not in _kind_name(assign):
                continue
            self._mark_consumed(assign, consumed)
            rhs = getattr(assign, "right", None)
            for lp in self._lvalue_bases(getattr(assign, "left", None)):
                if lp not in per_lhs:
                    per_lhs[lp] = []
                    order.append(lp)
                per_lhs[lp].append((labels, rhs))
        # A `default:` branch, if present, overrides the prior-assignment default.
        default_rhs: dict[str, Any] = {}
        dstmt = getattr(case, "defaultCase", None)
        if dstmt is not None:
            dassign = getattr(dstmt, "expr", None)
            if dassign is not None and "Assignment" in _kind_name(dassign):
                self._mark_consumed(dassign, consumed)
                for lp in self._lvalue_bases(getattr(dassign, "left", None)):
                    default_rhs[lp] = getattr(dassign, "right", None)
        for lp in order:
            drhs = default_rhs.get(lp)
            if drhs is not None:
                default_ep = self._emit_expr(drhs, logic_id, seen)
            else:
                default_ep = self._resolve_pending(
                    values.pop(lp, None), logic_id, seen, consumed
                )
            root = self._emit_case_chain(
                per_lhs[lp], case_expr, const_idiom, default_ep, logic_id, seen
            )
            if root is not None:
                values[lp] = ("node", root)

    def _emit_case_chain(
        self,
        branches: list[tuple[list, Any]],
        case_expr: Any,
        const_idiom: bool,
        default_ep: Optional[tuple],
        logic_id: int,
        seen: set,
    ) -> Optional[int]:
        """Fold `branches` (priority order) into a right-leaning 2:1 Mux chain over
        `default_ep`: each `_add_gate("Mux", ...)` gets the item predicate as `sel`,
        the branch value as `d1`, and the rest-of-chain (next item, then the default)
        as `d0`. Returns the outermost Mux id, or None if nothing was emitted."""
        acc = default_ep
        for labels, rhs in reversed(branches):
            nid = self._add_gate("Mux", rhs, logic_id, "Conditional")
            sel = self._case_select(case_expr, const_idiom, labels, logic_id, seen)
            self._wire_input(nid, sel, seen, "sel")
            self._wire_input(nid, self._emit_expr(rhs, logic_id, seen), seen, "d1")
            self._wire_input(nid, acc, seen, "d0")
            acc = ("node", nid)
        return acc[1] if acc is not None and acc[0] == "node" else None

    def _case_select(
        self,
        case_expr: Any,
        const_idiom: bool,
        labels: list,
        logic_id: int,
        seen: set,
    ) -> Optional[tuple]:
        """The Mux select for one case item. `case (1'b1)` uses each item predicate
        directly (the label *is* the boolean); a general `case (expr)` compares the
        controlling expr to each label with an equality `Cmp`. A comma-list item
        (`2'b00, 2'b01:`) ORs its per-label conditions."""
        conds: list[tuple] = []
        for lbl in labels:
            if const_idiom:
                ep = self._emit_expr(lbl, logic_id, seen)
            else:
                ep = self._emit_cmp_eq(case_expr, lbl, logic_id, seen)
            if ep is not None:
                conds.append(ep)
        if not conds:
            return None
        if len(conds) == 1:
            return conds[0]
        nid = self._add_gate("Or", labels[0], logic_id, "BinaryOr")
        for c in conds:
            self._wire_input(nid, c, seen)
        return ("node", nid)

    def _emit_cmp_eq(
        self, case_expr: Any, label: Any, logic_id: int, seen: set
    ) -> tuple:
        """An equality `Cmp` comparing the case controlling expr to a match label."""
        nid = self._add_gate("Cmp", label, logic_id, "Equality")
        self._wire_input(nid, self._emit_expr(case_expr, logic_id, seen), seen)
        self._wire_input(nid, self._emit_expr(label, logic_id, seen), seen)
        return ("node", nid)

    def _is_case_true(self, expr: Any) -> bool:
        """True for the `case (1'b1)` one-hot idiom — controlling expr is the literal
        `1'b1`, so each item label is itself the branch predicate (no comparator)."""
        e = expr
        while e is not None and (
            "Conversion" in _kind_name(e) or "Cast" in _kind_name(e)
        ):
            e = getattr(e, "operand", None)
        return self._literal_value(e) == "1'b1"

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

    # -- identifier occurrences (#225) ---------------------------------------
    def _enclosing_scope(self, nid: Optional[int]) -> Optional[str]:
        """Path of the nearest *ancestor* instance/interface of `nid` — the elaborated
        scope whose body physically contains this node's declaration. `None` at the
        top (a top instance's own name token is a module definition, not a member)."""
        cur = self.nodes[nid]["parent"] if nid is not None else None
        while cur is not None:
            node = self.nodes[cur]
            if node["kind"] in ("Instance", "Interface"):
                return node["path"]
            cur = node["parent"]
        return None

    @staticmethod
    def _name_ref_rel(sym_path: str, scope_path: Optional[str]) -> str:
        """A symbol path expressed *relative to* its enclosing elaborated scope.

        This is what makes one source span serve every instantiation of a module:
        `clk` inside `picorv32` is `clk` whether the reader is looking at
        `soc.g_lane[0].core` or `[1]`. A symbol that is not under the scope (a
        package parameter like `soc_pkg::XLEN`, a cross-hierarchy reference) is
        stored absolute with a leading `/` — a character SV paths never contain — so
        the consumer can tell the two apart without guessing.
        """
        if scope_path and sym_path.startswith(scope_path + "."):
            return sym_path[len(scope_path) + 1 :]
        return "/" + sym_path

    def _add_name_ref(
        self, sl: Any, length: int, cls: str, rel: str, end_off: Optional[int] = None
    ) -> None:
        """Record one identifier occurrence, deduped by (file, offset).

        The same span is reached once per elaborated instance of its module, so
        dedup keeps the record with the **shortest `rel`** — which is the one taken
        against the innermost enclosing instance, i.e. the correct scope. Equal
        `rel`s (the port/backing-net dual declaration) break by `_NAME_CLASS_RANK`.
        """
        if length <= 0 and end_off is None:
            return
        try:
            loc = self._loc(sl)
            fid = self._file_id(self.sm.getFileName(sl))
        except Exception:
            return
        span = length if end_off is None else end_off - loc["offset"]
        if span <= 0:
            return
        key = (fid, loc["offset"])
        prev = self._name_ref_index.get(key)
        if prev is not None:
            better = (len(rel), _NAME_CLASS_RANK.get(cls, 99)) < (
                len(prev["rel"]),
                _NAME_CLASS_RANK.get(prev["class"], 99),
            )
            if not better:
                return
        self._name_ref_index[key] = {
            "file": fid,
            "line": loc["line"],
            "col": loc["col"],
            "offset": loc["offset"],
            "len": span,
            "class": cls,
            "rel": rel,
        }

    def _scan_name_refs(self) -> None:
        """Collect every identifier occurrence the elaboration can classify (#225).

        Two sources, both authoritative:

        * **declarations** — each spine node's own name token (`sym.location` plus the
          name's length), classified by the node kind the walk already assigned;
        * **references** — every `NamedValue`/`HierarchicalValue` in an instance body,
          classified by the symbol slang resolved it to and spanned by the expression's
          own `sourceRange`. This is the span `_value_refs` computes and discards.

        An identifier the elaboration cannot classify simply yields no record: it stays
        default-colored, which is the point — no name guessing.
        """
        self._name_ref_index.clear()

        for nid, sym in self._decl_syms:
            node = self.nodes[nid]
            cls = _NAME_CLASS_BY_NODE_KIND.get(node["kind"])
            if cls is None or nid in self._modport_pin_ids:
                continue  # logic/gate nodes declare nothing; a modport pin is a view
            scope = self._enclosing_scope(nid)
            if node["kind"] in ("Instance", "Interface"):
                # The definition's own name token (`module picorv32`) — the same span
                # for every instantiation, so it dedups to one record. `rel` is empty:
                # the token names the scope itself, not a member of it.
                defn = getattr(getattr(sym, "body", None), "definition", None) or getattr(
                    sym, "interfaceDef", None
                )
                dname = getattr(defn, "name", None)
                if defn is not None and dname:
                    self._add_name_ref(defn.location, len(dname), "module", "")
                if scope is None:
                    # A top instance has no instantiation site — its `location` *is* the
                    # definition name token just emitted.
                    continue
            name = node["name"]
            loc = getattr(sym, "location", None)
            if name and loc is not None:
                self._add_name_ref(
                    loc, len(name), cls, self._name_ref_rel(node["path"], scope)
                )

        for nid, sym in self.instances:
            scope = self.nodes[nid]["path"]
            body = getattr(sym, "body", None)
            if body is None:
                continue

            def cb(n: Any, _scope: str = scope) -> None:
                k = _kind_name(n)
                if "NamedValue" not in k and "HierarchicalValue" not in k:
                    return
                target = getattr(n, "symbol", None)
                cls = _NAME_CLASS_BY_SYM_KIND.get(_kind_name(target)) if target else None
                path = _value_sym_path(target)
                if cls is None or not path:
                    return
                try:
                    sr = n.sourceRange
                except Exception:
                    return
                self._add_name_ref(
                    sr.start, 0, cls, self._name_ref_rel(path, _scope), sr.end.offset
                )

            try:
                body.visit(cb)
            except Exception:
                continue

        self.name_ref_rows = [
            self._name_ref_index[k] for k in sorted(self._name_ref_index)
        ]

    def build(self) -> dict[str, Any]:
        root = self.comp.getRoot()
        # Slang's analysis pass flags `always_comb` blocks that infer a latch; used
        # by `_logic_role` to classify them `Latch` rather than `Comb`.
        self._inferred_latch_locs = self._collect_inferred_latches()
        for top in root.topInstances:
            self._walk(top, None)
        edges = self._edges()
        self._apply_inits()
        # HLS C↔RTL provenance (#159): opt-in, so the default output is byte-identical.
        # Runs after the walk so every RTL file is registered before it tags languages.
        if self.hls_map:
            self._scan_hls_provenance()
            self._register_declared_c_sources()
        # Identifier-occurrence spans (#225): opt-in, and runs after the walk so every
        # node (and its declaring symbol) exists to classify against.
        if self.name_refs:
            self._scan_name_refs()
        model: dict[str, Any] = {
            "schema_version": SCHEMA_VERSION,
            "design": root.topInstances[0].name if root.topInstances else "",
            "generator": {"tool": "pyslang", "version": pyslang.__version__},
            "files": self.files,
            "nodes": self.nodes,
            "edges": edges,
            "enums": self.enums,
        }
        # Only emit source_map when the pass ran and found correspondences, so a
        # flag-off (or empty-result) run stays byte-identical to the pre-#159 output.
        if self.source_map:
            model["source_map"] = self.source_map
        # Same rule for name_refs (#225): absent unless the pass ran and found spans.
        if self.name_ref_rows:
            model["name_refs"] = self.name_ref_rows
        return model


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
    hls_map: bool = False,
    hls_comment_re: Optional[str] = None,
    hls_src: Optional[list[str]] = None,
    name_refs: bool = False,
) -> dict[str, Any]:
    return Elaborator(
        files,
        top,
        include_dirs,
        gate_level=gate_level,
        hls_map=hls_map,
        hls_comment_re=hls_comment_re,
        hls_src=hls_src,
        name_refs=name_refs,
    ).build()


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
    ap.add_argument(
        "--name-refs",
        action="store_true",
        help="Also emit identifier-occurrence spans (#225): every declaration name "
        "token and every resolved value reference, so the source pane can color "
        "identifiers by kind and a click on a usage resolves to the signal it names. "
        "Off by default; the default output is unchanged.",
    )
    ap.add_argument(
        "--hls-map",
        action="store_true",
        help="Also emit the HLS C/C++ <-> RTL provenance map (#159): scan the "
        "generated RTL's line-annotated comments for the originating C source and "
        "emit a source_map. Off by default; the default output is unchanged.",
    )
    ap.add_argument(
        "--hls-comment-re",
        metavar="REGEX",
        help="Override the HLS provenance-comment pattern (needs named groups "
        "'file' and 'line'). Only used with --hls-map.",
    )
    ap.add_argument(
        "--hls-src",
        action="append",
        default=[],
        metavar="PATH",
        help="Declare a C/C++ source FILE or a search-root DIRECTORY (#222). A file "
        "is registered even if no provenance comment references it, so an unmapped "
        "header is still browsable; a directory is searched when resolving a "
        "comment's C path. Repeatable. Only used with --hls-map.",
    )
    args = ap.parse_args(argv)

    files: list[str] = list(args.files)
    include_dirs: list[str] = list(args.include)
    seen: set[str] = set()
    for fl in args.filelist:
        parse_filelist(fl, files, include_dirs, seen)

    if not files:
        ap.error("no source files given (pass files and/or -f FILELIST)")

    el = Elaborator(
        files,
        args.top,
        include_dirs,
        gate_level=args.gate_level,
        hls_map=args.hls_map,
        hls_comment_re=args.hls_comment_re,
        hls_src=args.hls_src,
        name_refs=args.name_refs,
    )
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
        # newline="\n" so a regeneration on Windows produces the same bytes as on
        # Linux: the goldens are pinned LF (.gitattributes), and the CI staleness
        # check diffs them byte-for-byte. Without it Python's text mode would
        # translate every \n to \r\n and the golden would never match.
        with open(args.out, "w", newline="\n") as f:
            f.write(text + "\n")
    print(
        f"elaborated {model['design']}: {len(model['nodes'])} nodes, "
        f"{len(model['files'])} files",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
