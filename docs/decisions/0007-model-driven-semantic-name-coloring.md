# ADR 0007 — Model-driven semantic name coloring and usage resolution

- **Status:** Accepted; implemented (#225, PRs 1–5)
- **Date:** 2026-07-25
- **Deciders:** project maintainers
- **Relates to:** issue #225 (this change); [ADR 0008](0008-lexical-source-highlighting.md)
  (the lexical layer this sits on top of); fourth instance of the "additive projection over
  the same spine" pattern of [ADR 0005](0005-optional-gate-level-projection.md) and
  [ADR 0006](0006-hls-cpp-rtl-source-tracing.md)

## Context

Two problems in the source pane share one root cause.

**Coloring.** After #223 the pane tokenizes lexically: keywords, comments, strings, numbers.
Identifiers stay `plain`, deliberately — a lexer cannot tell a module from a signal from a
parameter without guessing, and guessing is what this project's governing principle forbids.
So the pane rendered a wall of undifferentiated identifiers.

**Usage clicks.** The model carried **declaration** spans only (`def_range`). Clicking `clk`
inside an `always_ff` block resolved by span to the *enclosing process node*, because that is
the narrowest node whose `def_range` covers the offset. The user clicked a signal and got a
process. Clicking the same identifier at its declaration worked fine — which made the gap
feel arbitrary rather than principled.

Both want the same missing fact: **where in the text does each identifier occur, and what
symbol does it name?** The elaboration already knows — slang resolves every value reference
and hands back a `sourceRange` the harness was computing and discarding.

## Decision

**Emit identifier occurrences from the elaboration as a first-class model field, and let
the model — never the tokenizer — say what an identifier *is*.**

### 1. Representation — additive, `schema_version` stays 1

A top-level `Document.name_refs: Vec<NameRef>`:

```
NameRef { file, line, col, offset, len, class: NameClass, rel }
NameClass { Module | Instance | Port | Signal | Param | Type
          | EnumMember | Function | Interface | Modport | Genvar }
```

- **`class` is read off the symbol slang resolved, never off the token text.** This is the
  whole point: `state` is a `Signal` or a `Type` or an `EnumMember` depending on what it
  binds to, and only the elaboration knows which.
- **`rel` is the symbol path relative to the enclosing elaborated instance** (`clk`,
  `g_lane[0].bus.valid`). One source span therefore serves **every instantiation** of its
  module — which is required, not merely economical: a generate-unrolled module has one
  source text and N elaborated instances. A symbol outside that scope (a package parameter,
  a cross-hierarchy reference) is stored **absolute with a leading `/`**, a character SV
  paths never contain, so the two cases are distinguishable without a flag.
- Records **dedup by `(file, offset)` keeping the shortest `rel`** — the innermost enclosing
  scope, which is the one a reader means.

Emission is behind **`--name-refs`**, following #157/#159: with the flag off the output is
byte-identical and `schema_version` stays `1`.

### 2. A separate index, deliberately not merged into `src_index`

`Design` builds a per-file `name_ref_index` (`Lapper`, structurally identical to `src_index`)
and **keeps it separate**. Merging them would be the obvious economy and would be wrong: a
usage span is *finer* than the declaration span that encloses it, so folding usages into
`src_index` would change which node is "narrowest covering" for every existing span query and
silently perturb the Phase-2 cross-probe that already works. Two indices, two questions.

### 3. Resolution precedence — the span anchor still wins

`CrossProbe::from_source` resolves by span first. If that anchors a concrete **leaf signal**
(`Var`/`Net`/`Port`) it is **trusted and returned** — a declaration click resolves through its
own `def_range`, and a generate-unrolled declaration reaches every lane because they share one
template span. Only when the span anchor is a **process or gate block** — the case that was
returning the wrong kind of object — does it consult `name_ref_at`, resolve the occurrence's
`rel` against each elaborated instance whose `def_range` covers the offset, and hand the hits
to the existing candidate picker. Sibling instances come back as `alternatives`, exactly as
generate ambiguity always has.

Any miss falls through to the span result, so **a model elaborated without `--name-refs`
behaves exactly as before.**

### 4. The frontend overlays, it does not replace

`names.ts` `applyNameRefs` takes the lexer's rows and the model's spans and **splits only the
`plain` token a ref lands in**. The lexer stays authoritative for keywords, comments, and
strings; the model is authoritative for identifiers; neither can overwrite the other. Spans
are fetched once per file (`name_refs` command → `NameRefDto`), not probed per token.

Semantic coloring is applied to the **RTL pane only**. The C/C++ pane stays lexical-only per
ADR 0006 — the harness never parses C, so there is no C symbol table to color from, and
inventing one would be precisely the heuristic both ADRs exist to prevent.

## Consequences

- **The invariant that makes cross-probe work is preserved.** A line's tokens must concatenate
  back to the original line exactly, or `srcoffset.ts`'s `lineColumn` computes the wrong byte
  offset and every source click lands in the wrong place. `applyNameRefs` preserves it, and
  that property is unit-tested on both the lexical and semantic layers.
- **`RKYV_FORMAT_VERSION` bumped 1 → 2.** Adding the field changed the archived `Document`
  layout, so every existing `.schemview_data/` cache is stale on first launch — detected by
  the header check, which falls back to JSON and rewrites. Correct by construction, but it is
  the first time a model field change invalidated the #21 cache, and it will not be the last.
- **The designlist path must pass the flag.** `elaborate_and_load` always passes
  `--name-refs` (as it does `--gate-level`, per #214): the frontend switches on data that has
  to already be in the model, so a design loaded from a `.f` file would otherwise silently
  lose both coloring and usage clicks with no error anywhere.
- **Cost is one bulk field.** The committed golden gains 2,388 records. Elaboration keeps
  a `sourceRange` it was already computing, so the harness cost is storage, not analysis.
- A symbol the elaboration cannot classify yields no record. The token then renders `plain`
  and resolves by span — degraded, never wrong.
