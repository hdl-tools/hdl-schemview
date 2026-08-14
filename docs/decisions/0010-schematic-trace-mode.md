# ADR 0010 — Trace mode: a seeded, boundary-crossing projection over the model spine

- **Status:** Accepted; implemented (#244 PR1–PR5, with #268/#269 folded in; amended by #285 and #293)
- **Date:** 2026-08-09
- **Deciders:** project maintainers
- **Relates to:** ROADMAP Phase 3c/3d and Phase 4; issue #244 (this change); issue #285 (§5); issue #293 (containment); consumes
  [ADR 0003](0003-storage-backend-for-parse-scalability.md)'s third outcome; follows the
  additional-projection pattern [ADR 0005](0005-optional-gate-level-projection.md) established
  over [ADR 0004](0004-internal-logic-schematic-granularity.md)

## Context

The schematic could only ever show **one scope at a time**. `scope_graph`'s own doc comment
states the limit plainly: *"Connections that leave the scope (e.g. to a top-level clock) are
omitted here; use `cone` to trace those."*

But `cone` had **no UI**. It was fully plumbed — `cone()` → `Session::cone` → the `cone` Tauri
command → `api.cone` — and *nothing in `app/src` called it*. The only live consumer was
`svxprobe graph --cone`. So an engineer chasing a signal across a module boundary had to guess
the parent scope, navigate there by hand, and re-find the net.

The question the hierarchical view cannot answer is the one debugging actually asks: **"what
drives this signal, and what does it reach?"** — which does not respect scope walls.

Two constraints shaped the answer before any code was written:

- **ROADMAP §Non-goals:** *"Do not render flat schematics of whole designs. Hierarchical,
  on-demand only."* Whatever this is, it cannot become a bulk flatten.
- **ADR 0003's third outcome.** The scalability benchmark found exactly one interactive miss
  in the whole matrix: `cone()` at **190.8 ms on a 59K-load clock**. ADR 0003 concluded the fix
  is *algorithmic* — level-of-detail and fan-out policy — not a storage swap, since no database
  makes a 59K-node cone cheap to **draw**. Trace mode is the first concrete consumer of that
  verdict, so caps are a requirement of the design rather than a later optimisation.

## Decision

**Add trace mode as an additional projection over the same model spine** — the pattern ADR
0005 established for gate level — selected per pane, defaulting off, leaving the hierarchical
view untouched.

A trace is **seeded on a signal and grown by explicit fan-in/fan-out expansion**, crossing
hierarchy boundaries as it goes. Its graph is "flat" only in the sense that it ignores scope
walls while following connectivity. It is **not** a bulk flatten of the design, so the ROADMAP
non-goal stands unamended.

Six sub-decisions define it — the last added by #285, after the first five shipped.

### 1. The trace is a list of steps, and is re-derived — never merged

A trace is an ordered list of `TraceStep { seed, dir, depth, fanout }`. The frontend holds that
list; every expansion re-sends the **whole** list and gets a **whole** graph back.

This is forced, not chosen. `PinAlloc` mints pin ids from a **per-call counter**, so the same
`(box, signal)` pair gets a different id in a second call — two graphs' pins collide, misaligned
with meaning. The graph DTOs are `Serialize`-only, so a graph cannot round-trip back from the
frontend to be merged into. One call per render is therefore the **only** place ids are
comparable.

Two properties fall out, both of which we would have wanted anyway:

- the backend holds **no per-pane trace state** — nothing to invalidate on a design or trace
  swap;
- a detached pane's trace is a plain `localStorage` value, so a pop-out restores its walk with
  no state to hand over.

The cost is re-walking on every expansion. It is affordable precisely because of §3: the walk
is capped, and the measured cost is ~5 ms on the committed golden.

### 2. Several steps become one graph by *sharing the walk's state*, not by merging

There is no merge pass anywhere. `emitted`/`emitted_pins` already dedupe boxes, `PinAlloc`
memoizes on `(box, signal)`, `seen_pairs` dedupes wires on the `(min, max)` pin pair, and
`visited` gates only the *next* hop — so a step re-seeded on ground an earlier step covered
still expands.

The load-bearing one is `stubs`. Because `follows` filters to one direction, **every directional
walk is one-sided by construction** and always synthesizes a signal stub. Sharing that set means
expanding a net's fan-in and then its fan-out re-uses the *same* stub: the net converges on one
junction node with its drivers on one side and its loads on the other, instead of being drawn
twice.

### 3. Caps are mandatory, and visible

`ConeLimits { depth: 4, fanout: 32, boxes: 500 }`, chosen from the `nav` benchmark rather than
by feel — the box budget from a measured **31 µs/box** on the golden, sized to one frame.

Truncation is an allowed **rendering** policy. Truncation the user cannot see is not. What a cap
drops is reported through `SchPort.more` (per pin) and `SchematicGraph.truncated` (per graph),
surfaced as a `+N` badge and a pane-level banner.

A `fanout` on a **step** overrides the shared budget for that step alone, which is what the
badge sends. Raising the *shared* budget instead would un-cap every signal at once, dragging a
global clock's whole fan-out onto a canvas nobody asked about. That override is deliberately
**not** clamped — exceeding the shared cap is its entire purpose — and `boxes` still bounds the
result, so "expand this signal fully" degrades to *"as far as the box budget allows, still
reporting the remainder"*.

Measured outcome at 1M nodes on the hot clock: **522.6 ms → 3.12 ms**, inside the 16 ms frame
budget, with the truncation reported rather than hidden. ADR 0003's cliff is answered.

### 4. The extractor is the scope graph's own machinery, seeded differently

Trace mode does **not** get its own notion of what a box or a pin is. `cone_box_of` resolves a
box by asking the scope graph's own `child_boxes` predicate, and `ScopeAnchors::pin_in_box`
resolves pins through the half of `scope_graph_with`'s `resolve` that stays true once the box is
known. `join_signal` — drivers × loads with `(min, max)` dedup — backs both views, so they
cannot drift on what counts as a connection.

This is a rule, not an implementation detail. The legacy `cone` re-implemented a subset of that
resolution, and **every node kind it special-cased was a latent divergence**: folded inverters
and constant ties drawn as phantom boxes, a bare interface bundle emitted edge-less, gates
inside an opaque `Ff` promoted to boxes. All three were found by a *general* cross-check —
`cone_with_agrees_with_the_scope_graph_on_what_is_a_box` — that the hand-written repro tests
missed. A future mode that changes what counts as a box must extend the shared predicate, never
fork it.

### 5. A module port is one signal, and a boundary is transparent in both directions (#285)

Amended after the fact. A port is **two model nodes at one canonical path with no edge between
them**: the `Port` carries only the external connection, the backing `Net`/`Var` every internal
one. §4's principle — *the extractor is the scope graph's own machinery, seeded differently* —
says nothing about which model nodes make up *one signal*, and the first cut walked the two
halves independently. Each got its own `join_signal` and its own anchor, so a traced port was
drawn **twice**, once on each side of the wall, in two disconnected components. That, not a
missing wall-crossing, is what "the trace stops at the boundary" actually was.

Three rules follow, and they generalize what `seed_signals` already did for a *seed*:

- **The same-path sibling group is the unit of the walk**, at every hop, not just at the seed.
  One budget, one anchor, one join. A crossing with both sides present routes drivers → anchor →
  loads rather than crossing them, so the two sides meet at the port instead of past it.
- **The group's representative is a pure function of the model** (its lowest member id), never of
  walk order — which is what lets the wall-crossing arm name the far side's anchor *before* that
  side is walked and still get the node the walk would later stand up.
- **A wall crossing materializes its anchor at the hop that reaches it.** The old
  `cone_box_of == None` arm's comment promised exactly that and only queued for the next hop, so
  a one-hop expansion — the UI's default — showed nothing, and a fan-in on any undriven input
  returned `{"nodes":[],"edges":[]}` with nothing set. An `Instance` node carries no edges of its
  own, so the same walk enqueues the port it landed on rather than the box, which is how a trace
  now descends into a module instead of dead-ending at it.

And one extension of §3: **absence is visible too.** A step's seed is always drawn and is exempt
from the orphan drop, so "nothing in this direction" is a picture rather than an empty canvas
indistinguishable from a failed lookup.

### 6. Cross-probe identity is unchanged

Every box keeps its model `NodeId`, every wire its `net_path`, every pin its canonical `path`.
A step names its seed by **path**, not by id — which is what a pin, a wire and a pop-out's
snapshot already carry. So Append-to-waveform and Show-in-source work in trace mode through
**no new resolution path**, and there is no name matching and no heuristic anywhere in the walk.

## Forces / rationale

- **Why not extend the hierarchical view instead?** The scope view answers *"what is inside this
  module"*. That is a different question, and the answer is a different graph. Overloading one
  view with both would have made the breadcrumb — which binds to a scope path — meaningless
  half the time.
- **Why per-pane rather than global?** #169 made schematic panes independent. A mode is a
  property of *a view of the design*, not of the design, so two panes must be able to disagree —
  one holding a scope while another chases a signal.
- **Why not persist the mode globally?** A restored mode with no steps is an empty canvas. Mode
  is carried only where the steps are carried too: the pop-out snapshot.
- **Why is the legacy `cone` still there?** It is the `svxprobe graph --cone` output contract
  *and* the `scale-bench` fan-out baseline that ADR 0003's 190.8 ms finding is measured against.
  Delegating it to a capped implementation would have overwritten the evidence for this ADR's
  own premise. It also has a **direction bug** the rebuild fixes (`Edge.dir` is relative to
  `e.port`, and the legacy filter ignores which side the walk stands on, so `cone(net, Dir::Out)`
  returns fan-in) — so the two are not output-compatible and must not be conflated.

## Consequences

- **The hierarchy the walk crosses is drawn, not flattened away (#293).** A boundary being
  transparent left a *flat* canvas — the logic behind a wall drawn as a peer of the logic
  outside it, with nothing saying which module it belonged to. `SchNode.parent` names the
  containing instance, `toElk` folds that into ELK compound nodes, and the renderer nests the
  SVG groups so transform composition does the coordinate maths. Two things had to be true for
  it to work: `elk.hierarchyHandling: INCLUDE_CHILDREN`, without which ELK refuses to route
  between levels at all; and rebasing each edge by the `container` ELK reports, because
  `elk.json.edgeCoords: ROOT` is *silently ignored* by elkjs 0.9.3 and correctness may not rest
  on an option that can go quietly missing.

- **The hierarchical view is unchanged.** `scope_graph`/`expand` output stays byte-identical,
  verified by dumping `svxprobe graph --json` across scopes and cones before and after each
  step and comparing. The layout gutter trace controls need is reserved only when the view asks
  for it, so scope-graph spacing does not move either.
- **`cone` gains a sibling, not a replacement.** `cone_with` is `trace_graph` with one step.
- **A latent defect was fixed on the way through.** `cone_with` could put one id on two nodes —
  a stub and a box, or a stub and a pin some box already exposed. Six of the committed golden's
  cone dumps carried a duplicate pin id, which no layout engine can anchor. The box wins, since
  every wire anchored on that id stays valid *because* it is the box's pin id. Guarded by
  `cone_with_never_repeats_a_node_or_pin_id`.
- **Phase 4's level-of-detail item is now demonstrated, not just argued.** `docs/benchmarking.md`
  §"The level-of-detail answer to that cliff" carries the numbers.
- **ADR 0003's #22 trigger is unaffected.** This is the algorithmic fix its third outcome called
  for; the storage question stays open and stays gated on a design that exceeds RAM.

## Out of scope

- **Bulk / whole-design flattening.** The ROADMAP non-goal stands. A trace is always seeded and
  always incremental.
- **Post-synthesis / netlist-level tracing** — [ADR 0001](0001-scope-rtl-vs-netlist.md).
- **Inline expand controls on every glyph.** `Assign` (#295), FF/latch and memory pins still
  have no pin element at all, so they can be reached only through the right-click menu on the
  *box*. Gate and mux pins were closed by #286; the FF is the awkward remainder, because its
  east gutter is not reserved — a dangling Q is labelled *inside* the box, so an outboard
  control means deciding whether that label moves out too.
