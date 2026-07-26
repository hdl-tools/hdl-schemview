# ADR 0004 — Internal-logic schematic granularity: process-level, not gate-level

- **Status:** Accepted
- **Date:** 2026-06-27
- **Deciders:** project maintainers
- **Relates to:** ROADMAP Phase 3d; issues #35 (epic), #31 (this change), #33, #34

## Context

Drilling into a **leaf** RTL module (one with no child instances — e.g.
`picorv32`) showed only its I/O frame, because the elaborated model held **no
internal-logic representation** for it. The harness emitted inferred-register
nodes for `always_ff` only; legacy `always @(posedge clk)`, `always @*`, and
continuous `assign` — the bulk of real RTL — were unmodelled (`picorv32`: 0
logic nodes). To render a leaf's internals as a schematic we must first put its
logic in the model, which forces a granularity decision: **how finely do we
decompose combinational/sequential logic into boxes?**

Two ends of the spectrum:

- **Gate / operator level:** decompose each expression into AND/OR/MUX/adder
  nodes. Faithful to a synthesized netlist, but explosive (one RTL line → many
  nodes), and off-niche — this is an *RTL* cross-probe tool, not a gate viewer.
- **Process / statement level:** one box per process or continuous assignment
  (`always` / `always_ff` / `always_comb` / `assign`), wired to the signals it
  reads and writes. Coarse, readable, and each box maps to exactly one source
  construct.

## Decision

**Model internal logic at process/statement granularity.** Each `always_ff`,
clocked/level `always`, `always_comb`/`always_latch`, and continuous `assign`
becomes **one** logic node:

- `NodeKind::Ff` — edge-sensitive sequential (`always_ff`, or legacy `always`
  with a `posedge`/`negedge`/`bothedges` event).
- `NodeKind::Comb` — combinational/latch (`always_comb`, `always_latch`,
  `always @*` / `@(a or b)`, continuous `assign`).

`initial`/`final` blocks, tasks, and functions are **not** logic nodes.
Gate/operator-level decomposition is **out of scope**.

## Forces / rationale

- **Single source of truth, preserved.** A logic node is a real model node with a
  `def_range` (its `always`/`assign` syntax), so clicking it cross-probes to
  source by lookup — no heuristics. Its wiring is `reads`/`assigns` resolved via
  pyslang value resolution, not string-matching. The schematic stays a projection
  of the elaborated model (per ADR/the governing principle).
- **Readability.** One box per source construct keeps a drilled leaf legible;
  gate-level would bury the user in inferred primitives.
- **Audience fit.** The user is debugging RTL behavior and expects a box to be the
  `always`/`assign` they wrote, at a known `file:line`.
- **No self-loops at source.** `data = reads − assigned − clock`, so a block that
  reads its own output (`q <= q + 1`) produces no self-edge.

## Consequences

- A new `NodeKind::Comb` is added across the schema, the Rust model, and the
  golden fixture (additive enum; `schema_version` stays `1`, mirroring how `FF`
  was added). Every crate that deserializes the golden gains the variant in the
  same change.
- The change is **behaviour-neutral for the schematic** until #33: `child_boxes`
  still excludes `Comb`, so hierarchical views are unchanged after this lands.
- Cross-block combinational feedback is a legitimate cycle (ELK lays out cyclic
  graphs); multiply-driven signals show full fan-in.
- A future gate-level mode, if ever wanted, would be an *additional* projection
  over the same model spine, not a replacement.

## Amendment — 2026-07-12 (#112, memory glyph)

Word-level **memory** rendering (#112) is added *within* this ADR's
process-level granularity, not against it:

- **`NodeKind::Memory`** is an additive kind (like `Comb`/`FF`; `schema_version`
  stays `1`) for a memory array (`logic [W-1:0] ram [0:N-1]`). The glyph maps to
  the **array `Var`'s own `def_range`**, so it is one modelled construct with a
  `file:line` — the single-source-of-truth invariant is preserved, exactly as for
  a process box. This is *not* the gate/operator decomposition rejected above.
- The **INIT marker** for `initial $readmemh` is carried as **metadata on the
  memory node** (`Node.init_source`), *not* as a logic node — `initial` stays
  non-logic per the Decision. The harness scans the `initial` block only to
  attribute the initializer to the array it targets.
- Memory pins (addr/din/dout/read/write) are derived from the **model edges**
  between the memory and the process(es) that access it (bounded array-access
  classification), not from expression-operator extraction.

**Still out of scope (deferred to #157 + a future ADR):** gate/mux/adder
primitives extracted from process expressions — the "AND/OR/MUX/adder"
decomposition this ADR rejects. That remains a separate *optional* gate-level
projection over the same spine, never the default drilled view.

> **Update — that ADR now exists:** [ADR 0005](0005-optional-gate-level-projection.md)
> (implemented, #157). It extends this one exactly as anticipated: an **opt-in**
> `Projection { ProcessLevel | GateLevel }`, with `ProcessLevel` remaining the default and
> the granularity this ADR fixes. Nothing here is retracted.
>
> One touch-point worth recording: #206 wired **memory-array read operands** into the
> gate-level muxes. A gate operand reading `ram[idx]` resolves to the whole `Memory` node
> this ADR introduced, and the memory box gained a synthesized read-out pin so the wire has
> somewhere to land. The array stays one glyph at both projection levels — the memory
> decision above is unchanged; gate level only gave its readers a visible connection.
