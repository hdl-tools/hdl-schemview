# ADR 0008 — Own source pane with a hand-rolled lexical highlighter

- **Status:** Accepted; implemented (#223 SystemVerilog, #224 C/C++)
- **Date:** 2026-07-23
- **Deciders:** project maintainers
- **Relates to:** issues #223, #224; closes the "Source editor" row of ROADMAP §3 and the
  "Editor" line of §8; [ADR 0007](0007-model-driven-semantic-name-coloring.md) layers
  semantics on top of this; [ADR 0006](0006-hls-cpp-rtl-source-tracing.md) is why a C/C++
  grammar exists at all

## Context

ROADMAP §3 lists "Source editor → adopt Monaco / CodeMirror, or an LSP client" with
**"Do NOT build: a code editor."** That instruction is right and this ADR does not overturn
it — but it answers a question the tool does not ask.

The source pane is **not an editor**. It never edits, never completes, never refactors, and
never validates. It does exactly two things:

1. Render a read-only file with line numbers.
2. Convert a click position into a **byte offset** that the cross-probe resolves against
   `src_index`.

Requirement (2) is the constraint everything else bends around. The offset the pane computes
must be the offset the model's `def_range` was computed against, exactly — a one-byte drift
puts the click in the wrong node. Before #223 a rendered line was a single text node, so a
caret offset *was* the column and this was trivially true.

Adding syntax coloring breaks that: a line becomes a run of `<span>`s. The question is not
"which highlighter is prettiest" but **"which option keeps the byte-offset contract
verifiable?"**

## Decision

**Keep the pane, and write a small per-language lexer (`app/src/syntax.ts`) that guarantees
a token-concatenation invariant.**

### The invariant

> A line's tokens, concatenated in order, reproduce the original line exactly.

This is unit-tested, and it is the entire reason the approach is safe. Given it,
`srcoffset.ts`'s `lineColumn` recovers the true column by summing the text length of the
tokens before the caret's, and `lineStarts[line] + column` remains the same offset the pane
produced when it emitted one text node per line. Coloring becomes a pure presentation change
with a mechanically checkable proof that it did not move anything.

### Why not Monaco or CodeMirror

Both are editors first. Adopting one means taking a large dependency, its own document model,
its own coordinate system, and its own DOM — and then reverse-engineering how its internal
positions map back to raw file byte offsets, per version. That mapping is the one thing this
pane must be certain about, and it is the thing an editor framework owns and is free to
change. The bundle and the API surface are secondary; the coordinate coupling is the
disqualifier. They also bring editing affordances that must then be disabled.

### Why not tree-sitter or a slang-based LSP

A parser would give real syntactic structure — but the tool **already has** something
strictly better for the part that matters: the elaborated model. Identifier semantics come
from the elaboration (ADR 0007), which knows what a symbol *binds to*, not merely how it
parses. A second, weaker source of truth for the same question is exactly the duplication
§2 exists to prevent. An LSP client would add a process boundary and a protocol to obtain
information already sitting in `Document`.

### Scope of the lexer — deliberately lexical only

`tokenizeLines` emits `keyword` / `type` / `number` / `string` / `comment` / `directive` /
`systask` / `operator` / `plain`. **Identifiers stay `plain`.** A lexer cannot distinguish a
module from a signal from a parameter without guessing, and the model answers that question
authoritatively — so it is left to ADR 0007's overlay rather than approximated here. The
whole-file scan carries `/* */` state across line boundaries so a multi-line comment
highlights on every line.

A `Grammar` carries per-language lexer traits — `directiveSigil` (`` ` `` vs `#`), `systask`
(SV only), `charLiteral` (C only; SV spends `'` on sized literals), `tickNumber` (SV only),
and its own number matcher. `grammarFor` keys off `SourceFile.language`, reusing **`csrc.ts`'s
`isCLanguage`** rather than re-listing the C tags, so the pane a file renders in and the
grammar it highlights with cannot disagree. An unrecognized language gets a keyword-less
fallback that still lexes comments, strings, and numbers — a safe default rather than
mis-coloring an unknown syntax.

Because both panes already render through the same `renderSourceInto`, **#224 required no
frontend change beyond the grammar** — the C pane inherited highlighting the moment the
grammar existed.

## Consequences

- **ROADMAP §3's "Do NOT build a code editor" stands.** Nothing here edits. If editing ever
  enters scope, Monaco/CodeMirror is still the answer and this pane should be replaced, not
  extended.
- **The lexer is small and permanently ours to maintain.** Two grammars, DOM-free, unit-tested
  — including the concatenation invariant. New languages are a `Grammar` literal, not an
  integration.
- **It will be imperfect on exotic syntax**, and that is acceptable: a mis-colored token is
  cosmetic, whereas a wrong byte offset is a wrong cross-probe. The invariant means the
  failure modes cannot trade places.
- **Two layers, one contract.** ADR 0007's semantic overlay must preserve the same
  concatenation invariant, and does. Every future layer on this pane inherits that obligation.
