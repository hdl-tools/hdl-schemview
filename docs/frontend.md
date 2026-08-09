# Frontend internals

How `app/src` is built. This is the *internals* companion to
[`app/README.md`](../app/README.md), which owns the module inventory (one line per file)
and the run/use instructions — start there if you want to launch the app rather than
change it.

Stack: vanilla TS + Vite 5 + Vitest, **no UI framework**. Schematic is SVG; waveform is
canvas 2D. TS is strict (`strict`, `noUnusedLocals`, `noUnusedParameters`); there is no
ESLint or Prettier, so match the surrounding style by hand.

Vitest's default environment is `node` (DOM-free). The suites that need a DOM
(`tree.test.ts`, `srcoffset.test.ts`) opt into `happy-dom` with a per-file
`// @vitest-environment` docblock, so every other suite stays fast.

## Layout and tabs

`index.html` is a CSS grid: a hierarchy tree top-left, a vertical `#col-splitter`
dragging the `--tree-w` track, a draggable `#row-splitter`, and two tab groups —
top-right `#content` (Source ↔ C/C++ ↔ Schematic ↔ Settings, **Source** active by
default) over bottom `#bottom-group` (Status ↔ Waveform, **Status** active by default).
Each `.tab-group` has a `.tabbar`; a tab's own controls ride in a per-tab `.tab-aux`.

`activateTab(panelId)` toggles `.active` within a group **and redraws the now-visible
view** — schematic and waveform containers have zero size while hidden, so
`refreshSchematic` re-fits a `schematicDirty` schematic and `redrawTracks` re-paints the
canvas. The Schematic and Waveform tab buttons start `hidden`; `activateTab` un-hides one
when it is activated, so the toolbar buttons and cross-probe actions reveal the on-demand
views.

The C/C++ tab and its file selector stay `hidden` until `source_files` reports a
non-SystemVerilog file — i.e. until an HLS design is loaded.

## The selection bus (`bus.ts`)

The single cross-pane coordination path. Right-click and tree handlers `publish` a
`Selection` (a resolved `ProbeResponse` plus which panes to reveal, or a scope path to
drill); one `subscribe(handleSelection)` in `main.ts` drives the panes.

- **One channel, two transports:** Tauri app-global `emit`/`listen` inside the webview, so
  it also reaches detached windows; a module-local fan-out in browser and test contexts.
  Chosen by `__TAURI_INTERNALS__` presence.
- **The payload carries model lookups, never geometry.**
- `ownsSelection` decides which window drives which pane, keyed on window id and the
  selection's `dest`:

  | Pane | Rule |
  | --- | --- |
  | `source` | Main window only |
  | `schematic`, `waveform` | An **addressed** selection drives exactly the window whose `self` label equals `dest` (main is addressable as `"main"`); a **broadcast** drives only main's own pane. A pop-out never follows a broadcast, and never follows another pop-out |

Pure builders `crossProbeSelection` / `scopeSelection` (both taking an optional `dest`),
plus `paneModeOf` and `ownsSelection`, are unit-tested.

## Detached panes

`windowMode = paneModeOf(location.search)` splits boot: `init()` runs the full app,
`initDetached(pane)` boots a single-pane window. The same page reloads with
`?pane=schematic|waveform&win=<label>` in a second Tauri window, where
`body.detached` / `body.detached-<pane>` CSS collapses the grid to that one pane
full-window.

- Every pop-out gets a **unique** label (`schematic-1`, `waveform-2`, …), so several
  coexist and each keeps its own scope, trace and picker. Main keeps its own pane — there
  is no handover.
- A waveform window's `selfLabel` **is** its backend `session_id`, so it loads and queries
  its own trace of the same design. Main drops a pane's session on `tauri://destroyed`.
- State is seeded from `localStorage` per label: `detach:<label>:scope` (a schematic
  pane's scope), `detach:<label>:wave` (a `WaveSnapshot`), and `detach:<label>:trace`
  (`{steps}`). A trace pop-out needs **both** trace and scope keys — the trace it shows and
  the scope its Hierarchy button falls back to. `storedTraceSteps` validates defensively,
  so a stale or hand-edited snapshot boots into hierarchy mode rather than throwing.
- On pop-out, `hideTab` closes the originating in-app tab; the toolbar button re-reveals
  it.
- **Gotcha:** the blanket `body.detached .pop-btn { display: none }` rule hides controls
  that a detached pane still needs. `#load-trace` and `#wave-pick-btn` each need an
  explicit `body.detached-waveform` escape hatch.

## Schematic pane

### Rendering

`elk.ts` turns a `SchematicGraph` into an ELK layout and then SVG DOM. `toElk`/`layout`
take an optional `LayoutOpts { affordances }` reserving an outboard `AFFORD_GUTTER` per
wall for trace mode's controls — a **view** property, not a graph one, so the hierarchy
view's spacing is untouched. It is applied once in `withAffordanceMargins` around the
per-kind dispatch rather than threaded through all six sizers, and it **parses and adds
to** any `elk.margins` a sizer already set (gate and mux set them for constant labels)
instead of clobbering it.

Gate-level primitives get IEEE distinctive glyphs: `renderGate`/`renderMux` draw the
flat-back-D AND, curved OR, notched XOR, N-variant output bubbles, Not/Buf triangles and
datapath op boxes, with wires meeting the shape directly and west input leads reaching the
actual back or arch. A folded inverter is an inversion bubble on the consumer's pin rather
than a separate box; a constant or parameter tie is a `const-label` just left of the pin's
west wall.

**Interface trunk wires.** The backend emits one `SchEdge` per member tap — that is the
cross-probe truth — and the frontend collapses each (raw port, consumer box, wall) group
into a single ELK edge anchored at a representative member pin (`trunkGroups`/`gatherBar`),
re-fanning the members via a gather bar just off the wall. Stubs cross-probe per member,
the trunk and bar cross-probe the bundle, and selection links both ways: a bundle lights
its stubs, a stub lights itself and the trunk, never its siblings.

### Trace mode

A `Hierarchy | Trace` segmented control in the schematic `.tab-aux`, wired from both
`init` and the `initDetached("schematic")` branch.

- `schemMode` and `traceSteps` are module-level, which **is** per-pane — a pop-out is its
  own webview and so its own JS context. They are deliberately not persisted globally,
  since a restored mode with no steps is an empty canvas.
- `renderTrace` sends the whole step list to `api.traceGraph` on every change (the backend
  re-derives rather than merges, so there is nothing to keep in sync) behind a `traceGen`
  staleness token, and **keeps the zoom instead of re-fitting** so a growing trace doesn't
  yank the canvas.
- `startTrace(path, dir)` **replaces** the list in hierarchy mode and **extends** it in
  trace mode — "Trace from here" means *this* signal, not this signal plus someone else's
  earlier walk.
- `setScope` is the **single** choke point where trace mode ends, covering the tree jump,
  a breadcrumb click and a box drill alike. The step list survives it, so Trace restores
  the walk.
- The breadcrumb doubles as the trace bar — same bar, same click-to-rewind, with a
  `Trace:` lead-in so the two are not confusable. `#trace-banner` surfaces
  `SchematicGraph.truncated` at pane level: a cap may drop connections, but never
  invisibly.
- `drawPinAffordances` draws the ◀/▶ expand button and the `+N` badge from `SchPort.more`,
  as **one helper called from each pin loop** rather than a copy per box shape (the loops
  have already drifted — an FF labels its dangling Q inside the box because its east gutter
  is unreserved). Both sit **offset off the pin's centre line**, because a wire arrives
  horizontally at exactly that point and a control on `py` would be drawn over the wire it
  expands. The badge sends `TRACE_FANOUT + more` as that step's `fanout`, which is why the
  pane passes its fan-out budget explicitly — it has to know the cap to ask for the
  remainder.

Step-list logic lives in `schempick.ts` and is pure: `pushTraceStep` appends unless an
identical `(path, dir, depth)` is present — re-clicking an affordance is idempotent, but
the *opposite* direction of the same signal is kept, since that is what makes a net's
fan-in and fan-out meet at one junction. `truncateTrace` rewinds to a bar entry, and an
out-of-range index is a no-op so a stale click cannot empty the canvas.

### The signal palette

Pressing **`a`** over a live schematic opens `#schem-palette`, scoped to the schematic's
*current* scope. It reuses `api.scopeSignals`, the shared `.snode` row styling and
`filterSignals` type-to-filter. Selecting a signal **publishes an addressed `["waveform"]`
cross-probe to the main window** rather than calling a local `addToWaveform` — that is
what lets a detached schematic pop-out, which has no waveform pane, still land the trace
in main. It also focuses the signal in the schematic itself. In trace mode it expands the
signal instead of appending a lane, and rows are never dimmed there, because
trace-presence has no bearing on traceability.

`refreshSchemPalette()` is called by both `setScope` **and** the `showInSchematic`
cross-probe drill (which bypasses `setScope`), guarded by a `paletteGen` token so a slow
`scopeSignals` for an old scope cannot clobber a newer one.

## Waveform pane

### Lane groups

Lanes live in `state.groups: WaveGroup[]` (`{ name, collapsed, waves }`); **there are no
loose lanes**, and a fresh pane is a single empty group.

Groups are user-authored containers, so the invariant is "keep every group + ensure a
trailing empty" — a group emptied by a move or remove is **preserved, not pruned**, and
the pane always ends with an empty group as the landing spot for a new one
(`normalizeGroups`/`workingGroupIndex`, re-enforced in `renderWaves`). New signals
accumulate in the working group (the last populated one). A group header's twist folds it
away, and **collapsed groups render no tracks** — `redrawTracks` and `markerTimeAt` map
canvases against `visibleLanes`, not `flattenLanes`. Deleting a group is enabled only when
it is empty, so a delete can never drop signals.

Dragging a lane by its name cell moves it within a group, across groups, or onto an empty
group (a tall dashed drop zone, since a bodyless header is too small to hit). The drop
lands where the accent line marks it; `moveLaneTo` is keyed by the stable lane `key`, so a
mid-drag re-render cannot move the wrong lane. A drag started on the name cell's column
resizer is suppressed.

### Lane identity

Because one signal can be several lanes ("add another view" stacks the same signal so it
can be read as hex *and* state name at once), **`ref` no longer identifies a lane**. Each
lane carries a stable `WaveTrace.key`, minted per window, round-tripped in the snapshot,
with counters reseeded on load via `laneCounterSeeds` so a pop-out never re-mints a
collision. Reorder and remove stay index-keyed; the picker's *added* mark stays
path-keyed.

`WaveTrace.path` carries the lane's canonical model node path, because a `signal_ref` is
trace-specific and the model path is the portable key. That is what lets `loadTraceOnly`
re-resolve every lane against a newly picked trace (`reresolveLane`), dropping signals the
new trace lacks and re-deriving sub-buses from `WaveTrace.slice` + the parent path.

### Geometry and reading

`wave.ts` owns time-window mapping, zoom/pan, segments, value-at-time, ruler ticks and the
per-trace/ruler canvas drawing. Native trace values are binary strings; `formatValue` and
`sliceBits` do conversion and slicing. Enum/FSM signals show the **state name** by default,
decoded from `WaveLink.enum_map` → `WaveTrace.enumMap` via `enumName`/`displayValue`, with
x/z and unmapped values falling back to the radix.

Left-click sets marker **A**, right-click marker **B**; the header shows A/B/Δ and the
value column reads each trace's value at A. A unit dropdown rescales the ruler and readout
via the trace's real timescale, while marker and window state stay in **raw ticks**.

Two CSS constraints that were bugs:

- The lane list is one flat CSS grid whose `align-content: start` packs lanes at the top —
  the grid's default `stretch` inflates the auto rows to fill a tall pane and spreads the
  lanes apart.
- The ruler stays pinned while lanes scroll only if **all four** ruler-row cells (the three
  `.wave-spacer`s and `.wave-ruler-cell`) are `position: sticky; top: 0` with an opaque
  `--bg`, since the ruler canvas is transparent.

### The signal picker

`#wave-picker` is a collapsible sub-column **inside `#wave-pane`**, toggled by
`#wave-pick-btn` or **Ctrl/⌘+B**. It lives inside the pane rather than in the window's
tree column precisely so every pop-out inherits it with no `body.detached` grid surgery.

`initPicker` builds the pane's tree via `createTree` on `state.top`; `pickSignal` is
`probeNode(path, null, sid)` → the **existing** `addToWaveform`, so dedupe, enum maps, lane
order and tab reveal are unchanged. A signal absent from the pane's trace is **dimmed and
inert, not pruned**.

The picker's tree deliberately does *not* `jumpToScope` — that broadcasts, and would drive
main's schematic from a pop-out. After a trace swap the design is unchanged, so the tree is
**not** rebuilt (that would collapse the user's expansion state); only the listed scope's
`in_trace` flags are refetched.

`#wave-picker[hidden] { display: none }` must outrank its own `display: flex`, or a
"collapsed" picker still renders.

## Source panes

`source.ts` `renderSourceInto` emits line rows with an `.ln` gutter, taking tokens from
`syntax.ts` piped through `names.ts`. Both the RTL and C/C++ panes render through it —
which is why adding the C pane needed no `main.ts` change.

**Two layers, neither overwriting the other:**

1. `syntax.ts` `tokenizeLines(text, lang?)` is **lexical only** — a whole-file scan so
   `/* */` block-comment state carries across line boundaries. `grammarFor` keys off
   `SourceFile.language`: absent or an SV tag ⇒ the SystemVerilog grammar; a C/C++ tag ⇒
   the C/C++ grammar; anything else ⇒ a keyword-less fallback that still lexes comments,
   strings and numbers, rather than mis-coloring an unknown language. A `Grammar` carries
   per-language traits (`directiveSigil`, `systask`, `charLiteral`, `tickNumber`, and its
   own `num` matcher) — SV spends `'` on sized literals, C on char literals.
   **Identifiers stay `plain`:** a lexer cannot tell a module from a signal without
   guessing ([ADR 0008](decisions/0008-lexical-source-highlighting.md)).
2. `names.ts` `applyNameRefs(lineTokens, refs)` overlays the model's identifier spans, each
   ref **splitting only the `plain` token it lands in** into a `name-<class>` token. The
   lexer stays authoritative for keywords, comments and strings; the model for identifiers
   ([ADR 0007](decisions/0007-model-driven-semantic-name-coloring.md)). Spans are fetched
   once per file, cached on the `state.source` entry, and gated on the `semanticNames`
   pref. **RTL pane only** — the C pane is lexical-only per ADR 0006.

> **Invariant (unit-tested in both modules):** a line's tokens concatenate back to the
> original line **exactly**. This is what keeps the byte-offset cross-probe correct.

Because a rendered line is now a run of token nodes rather than one text node, a caret
offset is no longer the column. `srcoffset.ts` `lineColumn(lineDiv, node, offsetInNode)`
sums the text length of the tokens before the caret's (skipping the `.ln` gutter) to
recover the true column, keeping `lineStarts[line] + column` — the offset the source
cross-probe resolves through — correct.

**Highlighting is by line number, not byte offset.** `SourceLoc` carries `line` and
`end_line`, and `highlightLineRange` lights `line..=end_line`, so Show-in-source lands on
the right line regardless of the `def_range` offset basis. An earlier byte-offset approach
assumed offsets were raw file bytes, which is wrong for the committed golden (LF-based), so
deep constructs drifted up on a CRLF checkout. `lineStarts` is retained only for the
source-*click* path, which shares the same offset basis, and a repo-wide
`.gitattributes eol=lf` pins RTL sources and goldens to LF on every platform so the working
tree matches.

`csrc.ts` routes each `SourceLoc` to the RTL or C pane. Its `isCLanguage` is the single
list of C/C++ language tags and is **reused by `syntax.ts`'s `grammarFor`**, so the pane a
file renders in and the grammar it lexes with cannot disagree.

## Trees

`tree.ts` is **the only DOM outside `main.ts` and `source.ts`**. It exports pure
`scopeFrames` (breadcrumb frames from a scope path) plus
`createTree({host, fetchChildren, onSelect, onActivate?})` → `{init, highlight, clear}` —
the factory behind both the window's `#hierarchy` tree and every waveform pane's picker
tree.

- `treeItems` is closed over **per instance**, so two trees coexist in one document. A
  module-level map let one tree's `init` orphan the other's rows and steal its highlight.
- `fetchChildren` is **injected** rather than importing `api`, so the module stays
  transport-free and each tree names its own session.
- Row CSS is `.tree`-scoped, not `#hierarchy`-scoped, so both trees are styled.

Navigation: a tree row's single click drives the schematic (`jumpToScope`); a double click
reveals the node in source, routed through the selection bus. In the source pane a
left-click moves the highlight to just the clicked line (a lightweight DOM `.hl`-marker
shift, not a whole-block re-highlight), while right-click opens the explicit
schematic/waveform cross-probe menu.

## Small pure modules

These are DOM-free and unit-tested, which is the point — the logic is testable without a
DOM environment:

| Module | Owns |
| --- | --- |
| `log.ts` | `formatTime` (`HH:MM:SS`) + `formatLogEntry` for the status pane |
| `prefs.ts` | `parseExcluded`/`formatExcluded`/`coerceExcluded` + thin localStorage wrappers (`excludedScopes`, `theme`, `semanticNames`, gate-level, picker-open) |
| `schempick.ts` | `filterSignals`, `isTextEntryTag` (so the bare `a` hotkey doesn't fire while typing), `moveIndex` (clamped, no-wrap, floors at 0), and the trace step list |
| `csrc.ts` | `isCLanguage`, `cSourceFiles`, and the RTL-vs-C pane decision |

## Status and logging

`log(level, message)` appends a timestamped, level-tagged row (`log-info`/`log-warn`/
`log-error`) to `#status-log`, auto-scrolls, and on `error` brings the Status tab forward.
Design-load progress, parse progress and API errors all route here. **The pane is the only
destination** — the toolbar carries inputs only, so there is one record rather than two.

## Settings

`initSettings` populates `#settings-pane` from `prefs.ts` and wires the controls back.
Theme applies live via `setTheme`; the semantic-names toggle re-renders in place through
`state.sourceView`; excluded scopes take effect on the next load. Theme vars live in
`style.css` (dark default, light via `:root[data-theme="light"]`), including the `--tok-*`
and `--tok-name-*` token colors for both themes.
