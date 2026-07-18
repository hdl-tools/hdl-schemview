# ADR 0005 — Optional gate-level projection over the model spine

- **Status:** Accepted
- **Date:** 2026-07-18
- **Deciders:** project maintainers
- **Relates to:** ROADMAP Phase 3d; issue #157 (this change); **amends/extends** [ADR 0004](0004-internal-logic-schematic-granularity.md); builds on #112/#160 (memory glyph, landed)

## Context

[ADR 0004](0004-internal-logic-schematic-granularity.md) locks the **default** drilled
internal-logic view at **process/statement granularity** — one box per `always` / `assign`
— and explicitly rejects gate/operator decomposition as the default. Its core invariant is
**one box = one source construct with a `def_range`**, so cross-probe stays a lookup.

ADR 0004 also sanctions the escape hatch this ADR takes: *"A future gate-level mode, if ever
wanted, would be an **additional projection** over the same model spine, not a replacement."*
Its amendment restates the deferral: *"gate/mux/adder primitives … remain a separate
**optional** gate-level projection over the same spine, never the default drilled view."*

Issue #157 is that projection. This ADR governs it. It exists because an engineer debugging
a leaf module sometimes wants to *see the logic* — the ANDs, the mux behind a `?:` — not just
the `always_comb` box that contains them. That view is opt-in and never displaces the
process-level default.

## Decision

**Add an optional, opt-in gate-level projection.** It is a second way to render the *same*
elaborated model, selected by a UI toggle (default **off**). With the toggle off, all
schematic output is **byte-identical** to today's process-level view. The projection is a
schematic-build choice threaded through `scope_graph`/`expand`; the golden model JSON is
unaffected unless the harness is explicitly run with a gate-level flag.

Four sub-decisions define it:

### 1. Granularity — associative collapse + muxes, composed

- **Associative operator chains** of the *same* operator collapse to **one N-input gate**:
  `a & b & c & d` → a single 4-input `And`. Reductions (`&x`, `|x`, `^x`) fold into the
  matching gate kind (a reduction-AND *is* an AND gate with one wide input).
- **`?:` and `if`/`else`** become a **`Mux`**, rendered as itself.
- The two compose recursively: `sel ? a & b : c & d` → a `Mux` whose two data inputs are
  each fed by a 2-input `And`.

Per-operator-node decomposition (`a & b & c` → two chained 2-input ANDs) is **rejected** —
it is the node explosion ADR 0004 warns against and reads nothing like the source RTL.

### 2. Node taxonomy — distinct kinds per operator (curated, IEEE distinctive shapes)

Distinct `NodeKind`s per gate type (an AND is not an OR), curated so operators that share a
glyph and meaning fold together rather than exploding the enum into every pyslang operator:

| `NodeKind` | Source operators | Glyph (IEEE Std 91 distinctive shape) |
|---|---|---|
| `And` / `Or` / `Xor` | `&` `\|` `^` chains + reductions | flat-back-D / curved-back / curved-back + notch |
| `Nand` / `Nor` / `Xnor` | `~&` `~\|` `~^`/`^~`, **and** a `~`/`!` folded onto a gate output | base gate shape **+ output bubble** |
| `Not` | `~`/`!` over a **leaf or non-gate** operand | **triangle + bubble** (inverter) |
| `Buf` | identity / unary `+` / explicit buffer | **triangle, no bubble** |
| `Add` / `Sub` / `Mul` | `+` `-` `*` (Div/Mod/Power → `Mul` box w/ op label, or deferred) | rectangular datapath box + symbol |
| `Cmp` | `==` `!=` `<` `>` `<=` `>=` | box labeled with the comparison |
| `Shift` | `<<` `>>` `<<<` `>>>` | box labeled `«`/`»` |
| `Mux` | `?:`, `if`/`else` | trapezoid; select on the south wall |

13 primitive kinds + `Mux`. Distinctive shapes are **SVG paths in the frontend renderer
only** (`main.ts`) — `elk.ts` sizes every primitive as a fixed box with `FIXED_POS` ports,
so the glyph shape does not affect layout. Precedent: `netlistsvg` / `d3-hwschematic` draw
these same distinctive shapes as SVG paths.

**NOT-folding rule.** A `~`/`!` unary directly wrapping a single associative gate chain
(`~(a & b)`) emits **one** `Nand`/`Nor`/`Xnor` (bubble folded onto the gate), not a separate
`Not`→`And`. A `~`/`!` over anything else (a bare signal, an arithmetic result, a mux) stays
a standalone `Not` inverter. This keeps the view reading like real logic instead of stacking
a redundant inverter on every gate.

### 3. Mux-select encoding — a new typed edge role

The Mux glyph must distinguish its **select/control** input (drawn on the south wall) from
its **data-branch** inputs (stacked on the west wall in branch order). Edge `dir` alone
cannot express this. Add a new optional field on `Edge`:

```
mux_port: Option<MuxPort>          // MuxPort { Sel, D0, D1, ... }
```

mirroring the `mem_port: Option<MemPort>` field #112 added for memories. This keeps
`Edge.select` meaning bit-slice only, is self-describing, and is immune to edge-emission
order (the harness edge set is an unordered dedup set). A new `PinRole::Sel` places the pin
on the trapezoid's south wall. *(Rejected: overloading `Edge.select` with a sentinel — a
stringly-typed dual meaning; and positional/emission-order inference — fragile against the
unordered edge set.)*

### 4. `def_range` — the scoped invariant relaxation

Each primitive maps to its **sub-expression's** pyslang `.sourceRange` (a span *within* a
line — e.g. the `a & b` fragment), fed through the existing `_loc`/`_file_id` machinery
(same shape as `_range_from_syntax`). This satisfies "has a `def_range`," so clicking a gate
still cross-probes to source by lookup — **no heuristics, no string-matching**, the governing
principle holds.

But a sub-expression span is a **fragment, not a whole source construct**. This is the one
deliberate relaxation of ADR 0004's "one box = one *source construct*" invariant, and it is
**scoped strictly to this opt-in projection**. The default process-level view keeps the full
invariant. Synthesized nodes get a synthetic path segment (`…$and{nid}`, `…$mux{nid}`)
extending the existing `_add_logic` `$ff{nid}` scheme.

## Forces / rationale

- **Additive, not a replacement.** ADR 0004's default is untouched; this is a second
  projection over the same spine, exactly as ADR 0004 pre-authorized.
- **Cross-probe stays a lookup.** Every primitive carries a real model `NodeId` + a
  `def_range`; wires carry `net_path`. Selection resolves by index, never by name-matching.
- **Readability over faithfulness-to-netlist.** Associative collapse and NOT-folding trade a
  little netlist purity for a view that reads like the RTL an engineer wrote — the audience
  fit that governs the whole tool.
- **Layout is unaffected.** Distinctive glyph shapes live in the SVG renderer; ELK sees
  fixed-size boxes with fixed ports, so incremental re-layout behaves exactly as for FF/mux.
- **Enum stays additive.** New `NodeKind`s and the `MuxPort` role are additive to the schema;
  `schema_version` stays `1`, mirroring how `Comb`/`Memory` were added.

## Consequences

- New `NodeKind`s (`And`, `Or`, `Xor`, `Xnor`, `Nand`, `Nor`, `Not`, `Buf`, `Add`, `Sub`,
  `Mul`, `Cmp`, `Shift`, `Mux`) + `MuxPort` + `PinRole::Sel` are added across the schema, the
  Rust model, the schematic crate, and the frontend DTOs (`app/src/types.ts`) in the same
  changes — the DTO-sync gate applies.
- The harness gains an opt-in gate-level extraction pass (a new operator-aware descent
  parallel to `_value_refs`), gated by a CLI flag. Default elaboration output is unchanged,
  preserving golden reproducibility in CI.
- `scope_graph`/`expand` gain a `Projection` parameter (default `ProcessLevel`); all existing
  callers pass the default, so behavior is neutral until the toggle flips.
- The matcher gate (≥95%) is unaffected — the projection is schematic-only; no wave signals
  change.

## Out of scope (unchanged from ADR 0004)

- The **default** drilled view stays process-level. Gate level is never the default.
- `initial`/`final` blocks, tasks, and functions remain non-logic.
- Post-synthesis / netlist-level tracing (ADR 0001) is still out of scope — this projects the
  *elaborated RTL's* expressions, not a synthesized netlist.
- `Div`/`Mod`/`Power` datapath primitives may be deferred to a later slice (rendered as a
  labeled `Mul`-family box or left process-level) without reopening this ADR.
