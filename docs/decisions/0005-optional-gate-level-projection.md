# ADR 0005 — Optional gate-level projection over the model spine

- **Status:** Accepted; implemented (#157 PR1–PR5, #199, #206, #207, #215)
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
- **Follow-up (#207) — LANDED** (`fc8b0bc`): `case`/`casez`/`casex` statements lower into
  priority-`Mux` chains (a one-hot `case (1'b1)` uses each item predicate as the select; a
  general `case (expr)` uses equality-`Cmp` selects), reusing the `Mux` kind + `mux_port`
  roles — additive, no schema change. The lowered branch assignments are excluded from the
  flat expression pass via a `consumed` set keyed on **source-range offsets**, never pyslang
  wrapper `id()`: pyslang re-wraps a node per traversal, so `id()` is unstable across the two
  passes and its reuse made node output hash-seed dependent.
- **Follow-up (#215) — LANDED**: §1's `if`/`else` → `Mux` completes the same pre-pass. An
  `if`/`else if`/`else` cascade folds into the same right-leaning `Mux` chain (condition →
  `sel`, branch value → `d1`, rest-of-chain → `d0`), and an `if` with no `else` falls through
  to the prior write, so a combinational `if` does not read as a latch. Two structural
  consequences: the pre-pass now **recurses into branch bodies**, which is what makes a `case`
  nested inside an `if` reachable at all (#207's walk was top-level only); and the wire-out
  moved to the *end* of the block, keyed on each l-value's final value, so a nested construct
  feeds its enclosing `Mux` instead of driving the signal a second time. Still additive —
  `schema_version` stays `1` and the flag-off output is byte-identical.
- A consequence worth recording: because the tally in `access_ports` counted *any* edge into a
  bare interface's members, the extra gate reads #215 surfaced flipped a bundle's access-port
  side **in the process-level graph**, which draws no gates. The tally now skips gate
  primitives, so a bundle's pin placement is projection-independent — the invariant #157
  claimed but had never been forced.

### As built — three refinements beyond the original slice

- **Constant and parameter operands are inline ties, not nodes (#199).** A hard-coded literal
  (`a & 8'hFF`) or a parameter (`a & MASK`) surfaces its value **on the gate's input pin**
  (`SchPort.constant`, drawn just outside the west wall) rather than as a separate source box.
  The value is then traceable *at the gate that uses it*, and a scope full of constants does
  not fill with boxes carrying no structure. A literal is emitted as a synthetic `Const` node
  purely to hang the value on; `Const` is never itself a box. A `{a, b}` concatenation operand
  does get a box (`Concat`), because it has internal structure a tie label cannot express.
- **Memory-array read operands wire to the array (#206).** A gate operand that reads
  `cpuregs[decoded_rs1]` resolves to the whole `Memory` node and the `Memory` box gains a
  synthesized east read-out pin, so the reader's wire reaches the glyph. Dropping the index is
  the same fidelity simplification as a peeled bit-select. Without this the operand vanished
  and the mux rendered with a missing input — which is worse than an approximate one.
- **Datapath `Div`/`Mod`/`Power` shipped**, as the out-of-scope note's first option: they map
  to the `Mul` node kind with the exact operator preserved on `Node.op`, so they render as a
  labelled Mul-family box. No separate node kinds were added.

## Out of scope (unchanged from ADR 0004)

- The **default** drilled view stays process-level. Gate level is never the default.
- `initial`/`final` blocks, tasks, and functions remain non-logic.
- Post-synthesis / netlist-level tracing (ADR 0001) is still out of scope — this projects the
  *elaborated RTL's* expressions, not a synthesized netlist.
- `Div`/`Mod`/`Power` datapath primitives may be deferred to a later slice (rendered as a
  labeled `Mul`-family box or left process-level) without reopening this ADR.
  **Resolved:** shipped as labelled `Mul`-family boxes (the first option) — the exact
  operator rides on `Node.op`, so no new node kinds were needed.
- **Function-call operands** are still not decomposed — the one genuine remaining gap. None
  occur in the committed golden, so it is untested rather than known-broken.
- **Syntax highlighting is lexical only** ([ADR 0008](0008-lexical-source-highlighting.md)) and
  therefore *not* an expression-level view of the source. The gate projection is the only
  expression-aware surface; the source pane deliberately does not attempt a second one.
