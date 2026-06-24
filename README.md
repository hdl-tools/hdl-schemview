# hdl-schemview

**An open, focused, RTL-level SystemVerilog cross-probe tool.**

`hdl-schemview` links three views of a digital design and keeps them in sync:

- **Source** — the SystemVerilog text (file:line:col, lexical scopes).
- **Schematic** — a generated, navigable diagram of the elaborated design.
- **Waveform** — simulation traces (VCD/FST, with user-supplied readers for others).

Click a signal in any view and the other two jump to the corresponding object.
The commercial equivalents are Synopsys Verdi and Cadence Indago; we are **not**
matching their breadth. We are building a focused, open, RTL-level tool by
composing existing best-in-class components.

## The four goals

1. **Full SystemVerilog elaboration** — via [slang](https://github.com/MikePopoloski/slang) (IEEE 1800-2023, through elaboration).
2. **Scalable visualization** — hierarchical, on-demand schematics that survive a real SoC.
3. **Accurate source / schematic / waveform cross-probing** — lookups against one elaborated model, not heuristics.
4. **VCD & FST support plus user-brought plugins** — built-in [wellen](https://github.com/ekiwi/wellen); user readers (e.g. FSDB) load out-of-process so no proprietary bits ship.

## The one principle that governs the design

**The elaborated hierarchy is the single source of truth. Source, schematic, and
waveform are three *projections* of it.** Source and waveform get bidirectional
maps *to* the elaborated model; the schematic *is* the elaborated model rendered.
Get this right and cross-probing is lookups, not guesswork.

## Status

Greenfield. The execution plan lives in **[docs/ROADMAP.md](docs/ROADMAP.md)**.
Architecture-level decisions are recorded as ADRs in
**[docs/decisions/](docs/decisions/)**.

The project go/no-go is the **Phase 1 matcher gate**: on a frozen reference
fixture, **≥ 95% of design-scope signals matched, with every miss attributable to
a named normalization-rule gap (zero mystery misses)**, against both FST and VCD.
No UI is built until that gate passes.

## License

MIT — see [LICENSE](LICENSE).
