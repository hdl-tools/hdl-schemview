# ADR 0002 — Distribution target: open-portable vs. FSDB-native

- **Status:** Accepted (locked for v1)
- **Date:** 2026-06-24
- **Deciders:** project maintainers
- **Relates to:** ROADMAP §4.2, Phase 5 (plugin surfaces)

## Context

Waveform input drives the distribution model. Two trace-format strategies are
mutually constraining:

- **Open-portable:** read VCD/FST (and GHW) via **wellen**, a pure-Rust,
  permissively-licensed library. The whole tool is standalone and redistributable
  with no third-party license obligations.
- **FSDB-native:** at a Synopsys VCS/Verdi shop, **FSDB** is the dominant trace
  format. Reading it requires linking Synopsys's **nFFR** libraries. nFFR is
  proprietary, undocumented, and **cannot be redistributed** — shipping it ties
  the tool to a licensed environment and forecloses an open release.

You cannot have "open + standalone" **and** "reads vendor traces natively" in one
binary. These are genuinely opposed: the redistribution freedom of the first is
exactly what linking nFFR destroys.

## Decision

**Build an open-portable core.** FSDB enters only as a **user-supplied,
IPC-isolated reader plugin**: the nFFR-linked reader is a *separate executable the
user builds against their own Verdi install*, which the tool talks to over
LSP/WCP-style JSON IPC. The tool itself ships **zero proprietary bits**.

## Forces / rationale

- **License cleanliness.** No Synopsys binaries or headers ever enter this repo or
  any release artifact (ROADMAP §6). The open release stays open.
- **The plugin boundary buys both properties.** An out-of-process reader gives a
  redistributable core *and* native FSDB access — without coupling them. This is
  the only structure that satisfies both goals.
- **Crash isolation for free.** A proprietary native lib crashing or leaking takes
  down only its subprocess, not the viewer.
- **WASM is unsuitable here.** A sandboxed WASM reader cannot `dlopen` a native
  proprietary library, so the FSDB reader must be a native subprocess, not WASM.
  (Value translators are the opposite case — see the split below.)
- **Minimal contract.** The only thing the cross-probe spine needs from any reader
  is hierarchical scope paths, so the matcher maps them to NodeIds. The
  `WaveSource` trait (ROADMAP Phase 5a) encodes exactly that.

## Consequences

- v1 ships with built-in VCD/FST/GHW via wellen and **no** FSDB support
  out-of-the-box.
- FSDB users follow a **bring-your-own-Verdi** path: build the reference nFFR
  reader against their licensed Verdi, then point the tool at it (ROADMAP Phase 6
  docs). The reference reader's exit gate is that it cross-probes *identically* to
  a native FST.
- The reader IPC protocol must be **versioned**; protocol churn is the main
  maintenance risk (ROADMAP §7, risk #4).
- This decision is deliberately separate from value-translator plugins (Phase 5b),
  which are sandboxed WASM/Python. **Do not merge the two plugin systems** — they
  have opposite constraints (native-linking vs. sandboxable).

## Loop-back

If a user reader's scope paths don't map cleanly, the fix lives in the Phase 1
normalizer, not in this boundary — the boundary is fixed.
