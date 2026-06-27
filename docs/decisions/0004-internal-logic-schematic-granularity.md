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
