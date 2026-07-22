# ADR 0006 — HLS C/C++ ↔ RTL source tracing via provenance comments

- **Status:** Accepted
- **Date:** 2026-07-22
- **Deciders:** project maintainers
- **Relates to:** issue #159 (this change); third instance of the "projection layer over the
  same spine" pattern established by [ADR 0001](0001-scope-rtl-vs-netlist.md) (netlist name-map)
  and [ADR 0005](0005-optional-gate-level-projection.md) (gate-level projection)

## Context

Some designs are not hand-written RTL — they are **generated from C/C++ by an HLS
(high-level synthesis) tool** (AMD/Xilinx Vitis HLS, Intel HLS, Bambu/PandA, LegUp/SmartHLS,
Catapult). For those users the SystemVerilog the tool emits is an intermediate artifact; the
source they reason about is the C/C++. Issue #159 asks: show the C/C++ source alongside the
RTL and let a click in either jump to the corresponding line in the other.

The governing principle of this tool is that **the elaborated hierarchy is the single source
of truth, and cross-probing is a lookup, not a heuristic** — "never reintroduce
guesswork/string-matching where a model lookup exists." A C↔RTL line correspondence recovered
by *name-matching or textual similarity* would violate that principle exactly the way ADR
0001 deferred netlist name recovery for the same reason.

So the design question is not "how do we render a C pane" (easy — it reuses the RTL line
list) but **"where does an authoritative C↔RTL correspondence come from?"**

## Decision

**Consume the line-annotated provenance comments HLS tools embed in the generated RTL.**

Every major HLS tool writes the originating C `file:line` into its generated RTL as a
comment (e.g. `assign x = a + b;  // foo.cpp:42`, or `// Operation 5 [foo.cpp:42]`). This is
the tool's *own* statement of provenance — authoritative, not inferred — and it is the single
most portable artifact across vendors (unlike the proprietary `.adb`/report-JSON/DWARF
databases, which are richer but need one ingester each). The harness scans it; the mapping is
a lookup from there on.

Concretely:

### 1. Representation — additive, `schema_version` stays 1

Two additions to the model contract, both following the #112/#157 precedent (new optional
fields, no version bump; default output byte-identical):

- **`FileEntry.language`** tags each source file (`"systemverilog"` for RTL, `"c"`/`"cpp"`
  for a referenced high-level source). Absent ⇒ SystemVerilog. Routes probes and picks a pane.
- **A top-level `source_map`** — a list of `{ generated: Range, source: Range }` line-region
  correspondences. Chosen over a per-node `hls_def_range` because the comment mapping is
  **coarse (RTL-line ↔ C-line) and symmetric**; a top-level table builds interval trees in
  *both* directions, reusing the existing `rust_lapper` machinery (`gen_map_index` /
  `src_map_index`), so a C↔RTL probe is the same interval lookup as source→node.

### 2. Cross-probe stays node-anchored

`CrossProbe::from_source` redirects a click in a C file through the provenance map to the
generated-RTL span, then resolves **the node(s) that span contains** (narrowest overlapping).
So a C-source click yields a normal node-anchored selection — and the C pane inherits the
schematic/waveform cross-probe *for free*, because it resolves to a real RTL node. The RTL→C
direction is `mapped_source` on the probe response (the C counterpart of the anchor's span).

### 3. Opt-in at the harness; discovered at the frontend

The pass is behind `--hls-map` (with an overridable `--hls-comment-re` for vendor-specific
comment forms), so non-HLS elaboration is byte-identical. The frontend enumerates
`source_files()` after a load and reveals the "C/C++ source" pane only when a non-SV file
exists — a pure-RTL design is entirely unaffected.

## Scope (this slice) and deferrals

**In:** the framework above + a thin vertical slice — schema, model indices, ingest
integrity, the harness comment scan, the C source pane with bidirectional line highlight,
and a synthetic fixture (`fixtures/hls_min/`). Granularity is **line-region** (the honest
resolution of a line comment).

**Deferred** (tracked follow-ups): tool-specific structured ingesters (Vitis `.adb`, Intel
report JSON, LLVM/DWARF `!dbg`) for sub-line precision; vendor comment-format presets; a
load-form UI to add C filelists explicitly (the slice discovers C files via the comments);
C-side syntax highlighting. C source is **display-only** — the harness never parses C
semantics, and never will; the correspondence is always the tool's own provenance, never
inferred.

## Consequences

- A C↔RTL trace is a model lookup, consistent with the governing principle.
- The mapping is only as good as the tool's comments; a tool that emits none yields an empty
  `source_map` and no C pane — a clean, honest no-op, never a guess.
- One more additive projection over the spine; the model spine's design (ADR 0001) again
  absorbs a new source level without a schema break.
