# Data model & DTOs

The shapes that cross every boundary in the tool: the elaborated node model the harness
emits, and the schematic DTOs the frontend renders.

See [architecture.md](architecture.md) for how these flow through the crates.

## The sync rule

> ⚠️ **Three representations of the same contract must stay aligned:**
>
> | Layer | File |
> | --- | --- |
> | Rust serde types (`gui`, `schematic` crates) | `core/crates/{gui,schematic}/src/lib.rs` |
> | TypeScript DTO interfaces | [`app/src/types.ts`](../app/src/types.ts) |
> | JSON Schema for the harness output | [`elaborate/schema/model.schema.json`](../elaborate/schema/model.schema.json) |
>
> Change a serde field without mirroring it in `types.ts` and the TS layer **silently**
> desyncs — there is no compile-time link between them. A model-level change usually
> needs the third edit too.

Model additions have so far been **additive**: `schema_version` is still `1`, and every
opt-in harness flag leaves the default output byte-identical.

## Model (`core/crates/model/src/lib.rs`)

### `NodeId`

A `u32` index into `Document::nodes`.

### `NodeKind`

```
Instance, Net, Port, Var, Param, ModuleDef, GenBlock, Ff, Comb, Latch, Assign,
Interface, Modport, Memory,
And, Or, Xor, Xnor, Nand, Nor, Not, Buf, Add, Sub, Mul, Cmp, Shift, Mux, Const, Concat
```

- **`Interface`** is an interface instance *or* a modport-specialized interface port;
  **`Modport`** is a named view of a bundle.
- **`GenBlock`** is one *elaborated* generate branch. The harness drops uninstantiated
  branches, so a discarded `if`-branch neither reparents phantom logic nor collides on the
  shared LRM-implicit name (`genblk1`).
  A `GenBlock` is a **navigable scope** — a tree row with its own schematic — only if its
  subtree holds a real design object (an `Instance` or a bare `Interface`). A
  **logic-only** block is a syntactic wrapper whose contents already render dissolved into
  the enclosing module, so `hierarchy_tree` / `scope_graph` / `scope_signals` reject it and
  a cross-probe onto its path walks *up* to the parent. The predicate is
  `is_navigable_scope`; the test is contents, not the generate keyword.
- **`Memory`** is a memory array (`logic [W-1:0] ram [0:N-1]` — the unpacked-dimension
  `Var`, re-kinded), drawn as a MEMORY glyph. Its addr/din/dout pins come from typed
  `Edge.mem_port` (`MemPort { Addr, Din, Dout }`) edges the harness emits by classifying
  `ram[idx]` accesses. Process granularity per
  [ADR 0004](decisions/0004-internal-logic-schematic-granularity.md) — the glyph maps to
  the array's `def_range`, so cross-probe stays a lookup.
- **The gate-level primitives** (`And`…`Concat`) are emitted only by the harness's opt-in
  `--gate-level` pass ([ADR 0005](decisions/0005-optional-gate-level-projection.md)). Each
  is a flat child of its process/assign node with a sub-expression `def_range`;
  associative chains collapse to one N-input gate, `~` folds onto the base gate, and a
  `?:` becomes a `Mux` whose three inputs are role-tagged on `Edge.mux_port`
  (`MuxPort { Sel, D0, D1 }`).

### `Node`

```rust
Node {
    id, name, path, parent, children, kind, symbol_key,
    def_range, inst_range, type_, dir, const_value, modport,
    drivers, loads, mem_depth, init_source, op, reset, enable, members,
}
```

| Field | Meaning |
| --- | --- |
| `op` | A datapath primitive's exact operator (`Cmp` → `"LessThan"`; `Divide`/`Mod`/`Power` on a `Mul`-kind node) — the label is a model fact, not a re-derivation |
| `mem_depth` / `init_source` | A `Memory`'s word count and its `$readmemh`/`$readmemb` INIT text |
| `modport` | The view name on a modport-specialized interface port (e.g. `mem`). Such a port carries directional `Port` children, one per modport member; each pin's `path` is the *underlying member's* canonical path |
| `members` | `Option<Vec<ModportMember { name, dir }>>` — per-modport membership on `Modport` nodes (descriptive; the modport stays a view). A member's own node resolves via `Design::modport_member_nodes` |
| `reset` / `enable` | Canonical paths of an inferred FF's async-reset signal and an inferred latch's gating signal. **Structural facts, never name guesses:** the reset is the timing-control edge whose signal the process body *also* reads (and `type_` then names the true clock); the enable is the sole signal read by the body's top-level conditional. Ambiguity ⇒ absent, so a sync reset stays untagged |

### A module port is two nodes

A module port is **two `Node`s sharing one canonical `path`, with no edge between them**, and
connectivity splits cleanly across the pair:

- the **`Port`** carries only the *external* connection — to the parent scope's net, or to the
  enclosing interface;
- the backing **`Net`/`Var`** carries every *internal* one, to the logic inside the module.

`Design::nodes_at_path` is the sanctioned recovery (`path_index` is one-to-many precisely for
this). Any consumer that asks "what is connected to this signal" **must fold over that lookup**,
or it sees one side of the wall and not the other: walking the two halves as separate signals is
what drew a traced port twice, once on each side, with nothing joining them (#285, ADR 0010 §5).

A modport-specialized port is the same shape one level out — it shares its path with the member
it views — but there the two *are* wired, and that edge is how a bare interface bundle is
reached (#269). So a group must not assume its members are unconnected.

### `Design` and `Document`

`Design { doc, path_index, src_index, conn_index, wave_index, gen_map_index,
src_map_index, name_ref_index }`.

- **`Document.enums: HashMap<String, EnumDef>`** — a normalized enum table keyed by
  canonical type string (matching `Node.type_`), where
  `EnumDef { width, members: Vec<EnumMember { name, value }> }`. Looked up via
  `Design::enum_for_type` and surfaced on `WaveLink.enum_map` for FSM state-name display.
- **`Document.source_map: Vec<SourceMapEntry { generated, source }>`** — the bidirectional
  line-region provenance map for HLS designs, with `FileEntry.language` tagging each source
  file (`"systemverilog"` / `"c"` / `"cpp"`; `None` ⇒ SV). Emitted only by `--hls-map`.
  `Design` builds `gen_map_index` / `src_map_index` (per-file `Lapper`s, symmetric to
  `src_index`); `mapped_from_gen` / `mapped_from_src` are the lookups, and
  `ProbeResponse.mapped_source` carries the C counterpart of an RTL anchor's span so one
  probe highlights both panes. C is **display-only** — never parsed; the correspondence is
  always the generating tool's own provenance, never inferred
  ([ADR 0006](decisions/0006-hls-cpp-rtl-source-tracing.md)).
- **`Document.name_refs: Vec<NameRef>`** — every identifier occurrence (declaration name
  tokens and resolved value references), each classified off the symbol the elaboration
  resolved, never the token text
  ([ADR 0007](decisions/0007-model-driven-semantic-name-coloring.md)):

  ```rust
  NameRef { file, line, col, offset, len, class: NameClass, rel }
  NameClass { Module, Instance, Port, Signal, Param, Type,
              EnumMember, Function, Interface, Modport, Genvar }
  ```

  `rel` is the symbol path **relative to the enclosing elaborated instance** (`clk`,
  `g_lane[0].bus.valid`), so one source span serves every instantiation; an out-of-scope
  symbol (a package param) is stored absolute with a leading `/`, a char SV paths never
  use. Records dedup by `(file, offset)` keeping the shortest `rel` — the innermost
  enclosing scope. Emitted only by `--name-refs`; adding the field bumped `ingest`'s
  `RKYV_FORMAT_VERSION` 1 → 2, since the archived `Document` layout changed.

## Schematic DTOs (`core/crates/schematic/src/lib.rs`)

```rust
SchematicGraph { root, nodes: Vec<SchNode>, edges: Vec<SchEdge> }   // + truncated
Side { West, East }                                                 // drives ELK port placement
```

### `SchNode`

```rust
SchNode { id, kind, label, path, expandable, ports: Vec<SchPort>,
          module, constant, modport, mem_depth, init_source, parent }
```

`parent` is the instance that **contains** this box, present only when the graph spans more
than one scope — i.e. trace mode (#293). Every other view *is* a single scope, so the frame
carries the answer and nothing nests. It names the nearest ancestor `Instance`; generate
blocks dissolve exactly as `child_boxes` dissolves them, and a box directly under the design
top has none, so a trace is never wrapped in one outer box that says nothing. `elk.ts` folds
these into ELK compound nodes and the renderer draws each as a labelled container.

`ports` is the **whole** port list from `scope_graph`, and only the pins the walk wired
from `cone_with`. A scope graph *is* that scope, so "here are this instance's connections"
is its content and every port belongs; a cone's content is "this is what the signal
reaches", where an untouched port is a pin, a label and a reserved gutter saying nothing —
and containment (#293) made that loud, since a traced module is pulled in whole. Pins
carrying `more` and a step's own seed survive the prune, and only `Instance`/`Interface`
are pruned: a gate's operands are the glyph's own shape, and an AND drawn with one input
would misstate the primitive.

`modport` marks an `Interface` node as a modport-qualified *port's* bundle — drawn as a
square frame pin at the view boundary rather than the hexagon bundle box an interface
*instance* gets. A bare bundle with `Modport` children reports `expandable`, and
`scope_graph`/`expand` on its path returns an **interface interior**: each `Modport` view
as a box, one directional pin per member, a wire for every member one view drives and
another reads, and a per-view boundary frame port marking its external face.

### `SchPort`

```rust
SchPort { id, name, side: Side, path, width, role, bundle, dangling, constant, more }
```

| Field | Meaning |
| --- | --- |
| `path` | The pin's canonical model path, so a right-click cross-probes it. Empty for synthetic const pins |
| `width` | Like `[31:0]`, else `None` — with an enum-table fallback (`lane_state_e` → `[1:0]`) |
| `role` | `PinRole { Clk, Reset, Enable, Addr, Din, Dout, Write, Read, Sel, Inv }`. Tags a synthesized FF/latch pin from model facts (`Node.type_` clock name, `Node.reset`, `Node.enable`) or a MEMORY glyph pin from `Edge.mem_port`. `Sel` is a mux's select input (south wall); `Inv` marks a folded inverter drawn as a bubble, whose `path` stays the *un-inverted* operand so cross-probe still lands on it |
| `bundle` | A whole-interface pin, drawn square instead of the directional triangle |
| `dangling` | Nothing connects to this pin — shown **dimmed rather than pruned**. A dangling FF Q or gate output gets a name label, since no wire labels it. Always `false` from `cone_with`: in a cone an unwired pin means "beyond the frontier", not "floating in the design" |
| `constant` | The inline tie value drawn just outside the pin's west wall when the operand is a literal or parameter |
| `more` | The count of connections a `ConeLimits` cap dropped — what the trace view's `+N` badge reads |

### `SchEdge`

```rust
SchEdge { id, source, target, net, net_path }
```

`net_path` is the connecting net's canonical model path (absolute, no bit-select), so a
wire click cross-probes via `probe_node`. It is `None` for synthetic constant tie-offs.

### Trace-view inputs

```rust
ConeLimits { depth, fanout, boxes }        // per-field serde defaults: {"fanout": 8} inherits the rest
TraceStep  { seed, dir, depth, fanout }    // fanout overrides ConeLimits.fanout for this step only
```

Semantics — and why `TraceStep.fanout` is deliberately unclamped — are in
[architecture.md](architecture.md) and
[ADR 0010](decisions/0010-schematic-trace-mode.md).
