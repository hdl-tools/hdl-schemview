# ADR 0001 — Cross-probe scope: RTL-level vs. netlist-level

- **Status:** Accepted (locked for v1)
- **Date:** 2026-06-24
- **Deciders:** project maintainers
- **Relates to:** ROADMAP §4.1, Phase 1 (go/no-go gate)

## Context

The tool keeps three views in sync against a single source of truth: the
elaborated hierarchy. The "source" view can be anchored at one of two levels, and
the choice changes how hard cross-probing is:

- **RTL-level:** the source text *is* the hierarchy. slang elaborates the RTL; a
  signal in the waveform maps back to an instance/net/var that exists verbatim in
  the user's source, at a known `file:line:col`.
- **Netlist-level:** the design has been synthesized. The waveform reflects gates,
  flops, and tool-mangled net names. Cross-probing to source must traverse a
  synthesis name-mapping (and optimizations like retiming, constant propagation,
  and net merging that have no clean source pre-image).

## Decision

**Build RTL-level first.** Netlist-level is a post-v1 extension, not part of the
v1 architecture.

## Forces / rationale

- **Direct cross-probe.** At RTL level, cross-probing is a lookup against the
  elaborated model — the whole premise of the project (ROADMAP §2). At netlist
  level it becomes heuristic name-recovery against a lossy transform.
- **Cheap validation.** The RTL path is a weekend-feasible `pyslang + wellen`
  spike with no UI. The Phase 1 gate proves or kills the matcher on real data
  before any rendering is built.
- **The hard risk is already the matcher.** Generate/param expansion and simulator
  naming are the #1 project risk even at RTL level. Adding synthesis name-mapping
  on top would stack the project's two hardest problems before either is proven.
- **Audience fit.** The target user is debugging RTL behavior, where source ↔
  waveform ↔ schematic correspondence is exact and expected.

## Consequences

- v1 assumes the waveform's scope hierarchy corresponds to the elaborated RTL
  hierarchy (modulo the normalization rules the Phase 1 matcher handles:
  testbench prefixes, `genblk` aliasing, escaped ids, array notation).
- Post-synthesis / gate-level traces are **out of scope** for v1 and will surface
  as unmatched signals in the matcher's visible miss list (never silently
  wrong) — acceptable and honest behavior.
- A future netlist-level mode would add a synthesis name-map projection layer
  *on top of* the same Node model, not replace it. The Node-model spine is
  designed so this is additive.

## Loop-back

If the Phase 1 gate fails and residual misses cannot be explained or ruled by
named normalization rules, **return to this decision** before building any UI —
the failure may indicate the chosen scope or fixture is wrong. This is the
project's cheap kill point.
