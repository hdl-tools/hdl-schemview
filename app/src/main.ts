// hdl-schemview frontend: three panes (schematic / source / waveform) linked by
// one selection, resolved through the cross-probe commands.
import { api } from "./api";
import {
  clampSegmentToRect,
  FF_LABEL_PAD,
  ffRole,
  fitZoom,
  gatherBar,
  IFACE_CAP,
  isLogicKind,
  layout,
  MEM_LABEL_PAD,
  nodeId,
  portId,
  trunkGroups,
  wireLabelPlacement,
} from "./elk";
import type { Pt, TrunkGroup } from "./elk";
import type {
  NodeRef,
  ProbeResponse,
  SchematicGraph,
  SchNode,
  SchPort,
  SignalEntry,
  StartupArgs,
  TraceTimescale,
  WaveLink,
} from "./types";
import { createTree, scopeFrames, type ScopeFrame, type TreeHandle } from "./tree";
import {
  crossProbeSelection,
  ownsSelection,
  paneModeOf,
  publish,
  scopeSelection,
  SELF,
  subscribe,
  type PaneMode,
  type RevealTarget,
  type Selection,
} from "./bus";
import { formatLogEntry, type LogLevel } from "./log";
import { lineRangeForSpan } from "./source";
import {
  formatExcluded,
  loadExcluded,
  parseExcluded,
  saveExcluded,
} from "./prefs";
import {
  defaultDisplayUnit,
  displayScale,
  displayValue,
  drawTrack,
  drawRuler,
  flattenLanes,
  laneCounterSeeds,
  maxTime,
  nearestEdge,
  normalizeGroups,
  panWindow,
  reresolveLane,
  sliceBits,
  TRACK_H,
  valueAt,
  valueAtMarker,
  visibleLanes,
  workingGroupIndex,
  xToTime,
  zoomWindow,
  type DisplayUnit,
  type Markers,
  type Radix,
  type TimeWindow,
  type WaveGroup,
  type WaveTrace,
} from "./wave";

const $ = (id: string) => document.getElementById(id)!;
const SVGNS = "http://www.w3.org/2000/svg";

// Append a timestamped, level-tagged row to the #status-log pane (#100, epic #94
// 4c) and echo the latest line to the compact toolbar #status. Errors also bring
// the Status tab forward, so a failure is never hidden behind the Waveform tab.
function log(level: LogLevel, message: string) {
  const entry = formatLogEntry(level, message, new Date());
  const pane = $("status-log");
  const row = document.createElement("div");
  row.className = `log-row log-${entry.level}`;
  const ts = document.createElement("span");
  ts.className = "log-ts";
  ts.textContent = entry.ts;
  const msg = document.createElement("span");
  msg.className = "log-msg";
  msg.textContent = entry.message;
  row.append(ts, msg);
  pane.appendChild(row);
  pane.scrollTop = pane.scrollHeight; // keep the newest entry in view
  $("status").textContent = entry.message; // compact latest-state echo
  if (level === "error") activateTab("status-pane");
}

// A saved viewport: zoom factor + scroll offsets, remembered per scope so
// breadcrumb-back restores the view you left rather than re-fitting.
interface ViewState {
  k: number;
  scrollLeft: number;
  scrollTop: number;
}

const state = {
  graph: null as SchematicGraph | null,
  stack: [] as ScopeFrame[],
  selected: null as number | null,
  source: new Map<number, string[]>(),
  // Signals pinned to the waveform pane, organized into collapsible groups (#182) with
  // one always-empty trailing group. Lanes append to the working group; reordered/removed
  // via the per-lane controls, regrouped via the name-cell menu. Reset on model reload.
  groups: [] as WaveGroup[],
  // Visible waveform time window (null = full window, derived from maxTime) and the
  // A (left-click) / B (right-click) markers. Reset on model reload (#16).
  waveView: null as TimeWindow | null,
  markers: { a: null, b: null } as Markers,
  // Trace timescale (tick → physical time) and the chosen display unit for the
  // ruler/readout. Fetched on load; null timescale → raw-tick display (#16).
  timescale: null as TraceTimescale | null,
  waveUnit: "ns" as DisplayUnit,
  // User-set widths (px) for the resizable name/value columns; undefined → the CSS
  // default (minmax). Persisted in localStorage (#84).
  waveCol: {} as { name?: number; value?: number },
  // The design main last loaded (#170), so a waveform pop-out can boot its own
  // session on the same model. Null until the first successful load.
  loaded: null as LoadSpec | null,
  // This window's loaded design top (#171), so the waveform pane's signal picker can
  // seed its tree in a pop-out too — which has no #hierarchy and no nav stack to read
  // it from. Null until loaded.
  top: null as string | null,
};

// Saved viewport per scope path, surviving stack pops so revisiting a scope
// (breadcrumb-back or re-drilling a sibling) restores the view you last had
// there. Only a first-ever visit falls through to zoom-to-fit. Cleared on
// model reload (a new design invalidates old viewports).
const viewCache = new Map<string, ViewState>();

// Net labels with the wire segments they ride, rebuilt each render. Their
// position/rotation is (re)computed by `placeWireLabels` so a label rotates to
// follow a vertical wire (#27) and stays on the visible portion of its wire as
// the view pans/zooms (#28).
let labelItems: { el: SVGTextElement; segs: [Pt, Pt][] }[] = [];

// The source pane's current file + per-line byte offsets (LF-based, matching the
// model's source ranges), so a right-click resolves to a file byte offset for
// `probe_source` (#19). Rebuilt by renderSource.
let sourceCtx: { file: number; lineStarts: number[] } | null = null;

const context = () => (state.stack.length ? state.stack[state.stack.length - 1].path : null);

// -- detached windows (#18 PR2) --------------------------------------------

// This window's role: the full app ("main") or a single popped-out pane, read
// from the launch URL (`index.html?pane=schematic`).
const windowMode: PaneMode = paneModeOf(location.search);

// This window's unique label (#169): "main" for the main window; a detached pane
// window is launched with `?win=<label>` (e.g. `schematic-2`) so that multiple
// independent schematic windows coexist. Falls back to the pane mode for a legacy
// single-pane window with no `win` param.
const selfLabel: string =
  new URLSearchParams(location.search).get("win") ||
  (windowMode === "main" ? SELF : windowMode);

// Monotonic counters for unique pop-out labels (main window only — only the main
// window creates pop-outs, so a local counter guarantees uniqueness). Schematic
// (#169) and waveform (#170) pop-outs are each independent windows.
let schematicPops = 0;
let wavePops = 0;

// A waveform pop-out owns its own backend session (#170), keyed by its window label,
// so it can load and query its own trace of the same design. The main window and
// schematic panes use the default "main" session (an undefined session id).
const sid: string | undefined = windowMode === "waveform" ? selfLabel : undefined;

// What the main window last loaded, captured so a waveform pop-out can boot its own
// session on the same design (#170): a pre-elaborated model, or a designlist to
// re-elaborate. Presence gates waveform detach.
type LoadSpec =
  | { mode: "model"; model: string; trace: string; excluded: string[]; srcRoot: string }
  | {
      mode: "filelist";
      filelist: string;
      top: string;
      incdirs: string[];
      trace: string;
      excluded: string[];
      srcRoot: string;
    };

// Per-label localStorage seed keys. The main window writes a pop-out's initial state
// before creating the window; the pop-out reads it synchronously on boot, so there is
// no event race. Schematic seeds its scope (#169); waveform seeds its load spec + lanes
// (#170).
const detachScopeKey = (label: string) => `detach:${label}:scope`;
const detachWaveKey = (label: string) => `detach:${label}:wave`;

// One seeded waveform pop-out: the design to (re)load under its own session, plus the
// lanes/view/markers main was showing at pop-out time.
interface WaveSnapshot {
  load?: LoadSpec;
  groups?: StoredGroup[]; // #182; pre-#182 snapshots carry `waves` instead (migrated on load)
  waves?: StoredTrace[];
  waveView?: TimeWindow | null;
  markers?: Markers;
  waveUnit?: DisplayUnit;
}

// A group serialized for a pop-out snapshot (#182): its lanes as StoredTraces.
interface StoredGroup {
  name: string;
  collapsed: boolean;
  waves: StoredTrace[];
}
function storeGroup(g: WaveGroup): StoredGroup {
  return { name: g.name, collapsed: g.collapsed, waves: g.waves.map(storeTrace) };
}
function loadGroup(s: StoredGroup): WaveGroup {
  return { name: s.name, collapsed: s.collapsed ?? false, waves: (s.waves ?? []).map(loadTrace) };
}
// Restore the pane's groups from a snapshot, migrating a pre-#182 flat `waves` list into
// a single group. Always normalized so the trailing empty group is present.
function loadGroups(snap: WaveSnapshot): WaveGroup[] {
  const groups = snap.groups
    ? snap.groups.map(loadGroup)
    : snap.waves
      ? [{ name: nextGroupName(), collapsed: false, waves: snap.waves.map(loadTrace) }]
      : [];
  return normalizeGroups(groups, nextGroupName);
}

// A WaveTrace carries an enumMap Map that JSON.stringify drops; (de)serialize it
// as pairs so a detached waveform window rebuilds the exact lanes main has.
interface StoredTrace {
  key?: number; // #179 lane identity; optional so a pre-#179 snapshot still loads
  ref: number;
  name: string;
  // Canonical model path of the signal (#170), so a lane survives a trace switch and
  // an addressed append can re-resolve it against a different session's trace.
  path?: string;
  slice?: { hi: number; lo: number }; // a sub-bus of `path` (#179)
  values: WaveTrace["values"];
  radix?: Radix;
  enumPairs?: [number, string][];
  showName?: boolean;
}
function storeTrace(tr: WaveTrace): StoredTrace {
  return {
    key: tr.key,
    ref: tr.ref,
    name: tr.name,
    path: tr.path,
    slice: tr.slice,
    values: tr.values,
    radix: tr.radix,
    enumPairs: tr.enumMap ? [...tr.enumMap] : undefined,
    showName: tr.showName,
  };
}
function loadTrace(s: StoredTrace): WaveTrace {
  return {
    key: s.key ?? nextLaneKey(), // mint one for a pre-#179 snapshot that lacks it
    ref: s.ref,
    name: s.name,
    path: s.path,
    slice: s.slice,
    values: s.values,
    radix: s.radix,
    enumMap: s.enumPairs ? new Map(s.enumPairs) : undefined,
    showName: s.showName,
  };
}

type DetachablePane = "schematic" | "waveform";

// Open (or focus, if already open) a pane in its own window. Seeds the window's
// initial state via localStorage, then creates a WebviewWindow that reloads
// index.html in `?pane=` mode; live cross-probing thereafter rides the bus.
async function popOut(pane: DetachablePane) {
  if (pane === "schematic" && !state.graph) {
    log("warn", "load a design before detaching the schematic");
    return;
  }
  if (pane === "waveform" && !state.loaded) {
    log("warn", "load a design before detaching the waveform");
    return;
  }
  // Each pop-out is an *independent* window with a unique label (#169 schematic,
  // #170 waveform), seeded once from main's current state; main keeps its own pane.
  const label =
    pane === "schematic" ? `schematic-${++schematicPops}` : `waveform-${++wavePops}`;
  if (pane === "schematic") {
    localStorage.setItem(detachScopeKey(label), context() ?? "");
  } else {
    const snap: WaveSnapshot = {
      load: state.loaded ?? undefined,
      groups: state.groups.map(storeGroup),
      waveView: state.waveView,
      markers: state.markers,
      waveUnit: state.waveUnit,
    };
    localStorage.setItem(detachWaveKey(label), JSON.stringify(snap));
  }
  try {
    const { WebviewWindow } = await import("@tauri-apps/api/webviewWindow");
    const w = new WebviewWindow(label, {
      url: `index.html?pane=${pane}&win=${label}`,
      title: `hdl-schemview — ${pane}`,
      width: pane === "schematic" ? 1000 : 1200,
      height: pane === "schematic" ? 800 : 500,
    });
    w.once("tauri://error", (e) =>
      log("error", `detach failed: ${JSON.stringify(e.payload)}`),
    );
    // A waveform pop-out owns its own backend session (#170); drop it when the window
    // closes so the design/trace it loaded is freed.
    if (pane === "waveform") {
      void w.once("tauri://destroyed", () => void api.unloadDesign(label));
    }
  } catch (e) {
    log("error", `detach failed: ${e}`);
  }
}

// -- load ------------------------------------------------------------------

// Which design-input flow the toolbar is set to: a pre-elaborated model JSON,
// or a designlist (.f) elaborated on load by the external harness (#93).
function loadMode(): string {
  return ($("load-mode") as HTMLSelectElement).value;
}

// Show the inputs for the selected flow (model path vs filelist/top/incdirs).
function syncLoadMode() {
  const filelist = loadMode() === "filelist";
  $("model").classList.toggle("hidden", filelist);
  for (const id of ["filelist", "top", "incdir"]) $(id).classList.toggle("hidden", !filelist);
}

async function load() {
  const model = (($("model") as HTMLInputElement).value || "").trim();
  const trace = (($("trace") as HTMLInputElement).value || "").trim();
  const srcRoot = (($("srcroot") as HTMLInputElement).value || ".").trim();
  const excluded = loadExcluded();
  // Elaboration can take seconds; block re-entry until this load settles — the
  // disabled flag also guards against the auto-load (#136) racing a manual click.
  const button = $("load") as HTMLButtonElement;
  if (button.disabled) return;
  button.disabled = true;
  try {
    let top: string;
    if (loadMode() === "filelist") {
      const filelist = (($("filelist") as HTMLInputElement).value || "").trim();
      const topName = (($("top") as HTMLInputElement).value || "").trim();
      const incdirs = (($("incdir") as HTMLInputElement).value || "")
        .split(";")
        .map((s) => s.trim())
        .filter(Boolean);
      log("info", `elaborating ${filelist || "designlist"}…`);
      top = await api.elaborateAndLoad(filelist, topName, incdirs, trace, excluded, srcRoot);
      // Capture the load so a waveform pop-out can re-elaborate its own session (#170).
      state.loaded = { mode: "filelist", filelist, top: topName, incdirs, trace, excluded, srcRoot };
    } else {
      log("info", `loading model ${model}…`);
      top = await api.loadDesign(model, trace, excluded, srcRoot);
      // Capture the load so a waveform pop-out can load its own session (#170).
      state.loaded = { mode: "model", model, trace, excluded, srcRoot };
    }
    log("info", `loaded ${top}`);
    state.stack = [];
    viewCache.clear();
    // A new design invalidates the old traces (signal_refs are model-specific). Reset to
    // a single empty group (#182), the pane's default view.
    state.groups = [];
    normalizeWaveGroups();
    state.waveView = null;
    state.markers = { a: null, b: null };
    state.timescale = await api.traceTimescale();
    state.waveUnit = defaultDisplayUnit(state.timescale);
    syncUnitSelect();
    renderWaves();
    state.top = top;
    await initHierarchy(top);
    await initPicker();
    await setScope(top, top);
  } catch (e) {
    log("error", `load failed: ${e}`);
  } finally {
    button.disabled = false;
  }
}

// A scope with a cached viewport is restored to it; a first-time scope is
// zoom-to-fit and scrolled top-left (see renderSchematic).
async function setScope(path: string, label: string, push = true) {
  const graph = await api.scopeGraph(path);
  state.graph = graph;
  if (push) state.stack.push({ path, label });
  renderBreadcrumb();
  highlightTree(path);
  await renderSchematic(graph, viewCache.get(path));
}

// Snapshot the current schematic viewport so it can be restored later.
function captureView(): ViewState {
  const host = $("schematic");
  return { k: zoom.k, scrollLeft: host.scrollLeft, scrollTop: host.scrollTop };
}

// Stash the on-screen scope's viewport (keyed by path) before navigating away,
// so returning to it restores zoom + scroll instead of re-fitting.
function rememberCurrentView() {
  const cur = state.stack[state.stack.length - 1];
  if (cur) viewCache.set(cur.path, captureView());
}

function renderBreadcrumb() {
  const bc = $("breadcrumb");
  bc.innerHTML = "";
  state.stack.forEach((f, i) => {
    const s = document.createElement("span");
    s.textContent = f.label;
    s.onclick = () => {
      rememberCurrentView();
      state.stack = state.stack.slice(0, i);
      setScope(f.path, f.label);
    };
    bc.appendChild(s);
    if (i < state.stack.length - 1) bc.appendChild(document.createTextNode(" / "));
  });
}

// -- hierarchy tree (#92) ----------------------------------------------------

// The window's hierarchy tree, in the main window only (a detached pane has no
// #hier-pane). Rows are rendered by `createTree`, which the waveform pane's signal
// picker also uses (#171).
let hierTree: TreeHandle | null = null;

// Build (or rebuild, on model reload) the tree pane: the design top plus its
// direct children; deeper levels are fetched lazily on expand.
async function initHierarchy(top: string) {
  hierTree ??= createTree({
    host: $("hierarchy"),
    // `sid` is undefined in the only window that has this tree, so this is the main
    // session — passing it just makes that explicit rather than implicit.
    fetchChildren: (path, depth) => api.hierarchyTree(path, depth, sid),
    // Clicking the row navigates the schematic to this scope.
    onSelect: (node) => jumpToScope(node.path),
    // Double-clicking reveals the node's module/instance in the source pane (#164):
    // probe its canonical path → source def → block-span highlight, complementing the
    // single-click schematic nav so the tree drives both views.
    onActivate: (node) =>
      void api
        .probeNode(node.path, context(), sid)
        .then((r) => r && publish(crossProbeSelection(r, ["source"]))),
  });
  await hierTree.init(top);
}

// Navigate to an arbitrary scope from the tree. Routed through the selection bus
// (#18) so it drives the schematic wherever it lives — the same window or a
// detached one; the schematic-hosting window runs `navToScope`.
function jumpToScope(path: string) {
  void publish(scopeSelection(path));
}

// -- waveform signal picker (#171) -------------------------------------------
//
// Every waveform pane carries its own picker — main's and each pop-out's — so a pane
// populates itself instead of waiting for another window to address lanes at it. It is
// scoped to this window's session (`sid`), so a pop-out lists its own trace's answers.
//
// It deliberately does *not* touch the schematic: a tree row here shows that scope's
// signals, it doesn't drill. (`jumpToScope` broadcasts, which would drive main's
// schematic from a pop-out — not what a signal picker means.)

let pickTree: TreeHandle | null = null;
// The scope whose signals are listed, so a trace swap can refresh their in_trace flags
// without rebuilding the (design-derived) tree.
let pickerScope: string | null = null;
let pickerSigs: SignalEntry[] = [];

function setupPicker() {
  $("wave-pick-btn").addEventListener("click", () => togglePicker());
  // Ctrl/⌘+B — the sidebar-toggle convention, and no letter key is otherwise bound.
  // Gated on a visible waveform pane: in main it fires only while that tab is forward;
  // in a detached waveform window the pane is always active, so it always fires.
  document.addEventListener("keydown", (e) => {
    if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== "b") return;
    if (!$("wave-pane").classList.contains("active")) return;
    e.preventDefault();
    togglePicker();
  });
  togglePicker(loadPickerOpen());
}

function togglePicker(open?: boolean) {
  const el = $("wave-picker");
  const show = open ?? el.hidden;
  el.hidden = !show;
  $("wave-pick-btn").setAttribute("aria-expanded", String(show));
  try {
    localStorage.setItem("wavePickerOpen", show ? "1" : "0");
  } catch {
    /* private mode / quota — the picker just won't remember */
  }
  // #wave-list resizes, and setupResizeRedraw's observer redraws the canvases for us.
}

// Closed by default, like every other on-demand surface here.
function loadPickerOpen(): boolean {
  try {
    return localStorage.getItem("wavePickerOpen") === "1";
  } catch {
    return false;
  }
}

// (Re)build this window's picker. The tree is design-derived, so it survives a trace
// swap untouched — only the signal list's in_trace flags move (see loadTraceOnly).
async function initPicker() {
  pickerScope = null;
  if (!state.top) {
    pickTree?.clear();
    renderSignalList([]);
    return;
  }
  pickTree ??= createTree({
    host: $("wave-picker-tree"),
    fetchChildren: (path, depth) => api.hierarchyTree(path, depth, sid),
    onSelect: (node) => void showScopeSignals(node.path),
  });
  await pickTree.init(state.top);
  await showScopeSignals(state.top);
}

// List `scope`'s signals, resolved against *this* pane's trace.
async function showScopeSignals(scope: string) {
  pickerScope = scope;
  pickTree?.highlight(scope);
  try {
    renderSignalList(await api.scopeSignals(scope, sid));
  } catch (e) {
    log("error", `scope signals failed: ${e}`);
  }
}

function renderSignalList(sigs: SignalEntry[]) {
  pickerSigs = sigs;
  const host = $("wave-picker-sigs");
  host.innerHTML = "";
  if (!state.top) return;
  if (!sigs.length) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = "(no signals in this scope)";
    host.appendChild(empty);
    return;
  }
  // Lanes already pinned, keyed by model path (#170's WaveTrace.path) — so a re-click
  // reads as a no-op rather than looking broken (addToWaveform dedupes silently).
  const added = new Set(laneList().map((w) => w.path).filter(Boolean));
  for (const s of sigs) {
    const row = document.createElement("div");
    row.className = "snode";
    if (!s.in_trace) row.classList.add("dim");
    if (added.has(s.path)) row.classList.add("added");
    const name = document.createElement("span");
    name.className = "sname";
    name.textContent = s.name;
    row.appendChild(name);
    if (s.width) {
      const w = document.createElement("span");
      w.className = "swidth";
      w.textContent = s.width;
      row.appendChild(w);
    }
    row.title = s.in_trace ? s.path : `${s.path} — not in this trace`;
    if (s.in_trace) row.onclick = () => void pickSignal(s.path);
    host.appendChild(row);
  }
}

// A picker click is the existing append path: resolve the model path against this
// window's session, then hand the WaveLink to addToWaveform — same dedupe, enum map,
// lane order and tab reveal as the right-click menu. Null context, like
// appendResolved/loadTraceOnly: the path is absolute and canonical.
async function pickSignal(path: string) {
  try {
    const r = await api.probeNode(path, null, sid);
    if (r) await addToWaveform(r.wave, path);
  } catch (e) {
    log("error", `probe failed: ${e}`);
    return;
  }
  renderSignalList(pickerSigs); // refresh the "added" marks
}

// Drill the schematic into `path`: the breadcrumb becomes the scope's ancestor
// chain (not the drill-down history), then the schematic loads it like any other
// navigation. Runs in whichever window hosts the schematic (bus handler).
function navToScope(path: string) {
  rememberCurrentView();
  const frames = scopeFrames(path);
  state.stack = frames.slice(0, -1);
  // Tree navigation is a "show me this scope" action → surface the schematic tab
  // (source is the default, on-demand layout) before it renders (#99).
  activateTab("schematic-pane");
  setScope(path, frames[frames.length - 1].label);
}

// Keep the tree's selection in step with whatever sets the scope (tree click,
// schematic drill, breadcrumb). A scope deeper than the fetched tree has no
// row yet — the highlight simply clears until that branch is expanded. A detached
// pane has no tree at all, hence the null guard.
function highlightTree(path: string) {
  hierTree?.highlight(path);
}

// -- schematic -------------------------------------------------------------

// Zoom bounds shared by manual zoom (setZoom) and explicit fit (fitView, #114).
const MIN_ZOOM = 0.2;
const MAX_ZOOM = 6;

// Current zoom factor. Manual zoom (setZoom) mutates it in place; a scope change
// resets it via renderSchematic's zoom-to-fit (or restores a saved view on back).
const zoom = { k: 1 };

// The schematic renders into #schematic even when its tab is hidden (clientWidth
// 0 → a degenerate fit). renderSchematic sets this when it draws while hidden; the
// next schematic-tab activation re-fits against the now-visible pane (#99).
let schematicDirty = false;

async function renderSchematic(graph: SchematicGraph, restore?: ViewState) {
  const host = $("schematic");
  host.innerHTML = "";
  if (!graph.nodes.length) {
    host.textContent = "(empty scope)";
    return;
  }
  const laid: any = await layout(graph);
  const baseW = Math.max(laid.width ?? 400, 200);
  const baseH = Math.max(laid.height ?? 300, 150);
  const svg = document.createElementNS(SVGNS, "svg");
  svg.dataset.baseW = String(baseW);
  svg.dataset.baseH = String(baseH);
  // All content lives in one <g> so a single transform zooms everything.
  const root = document.createElementNS(SVGNS, "g");
  svg.appendChild(root);

  // 1. Wires (under everything). A net name is drawn once even though the net may
  //    fan out over many wires; we accumulate *all* of that net's segments under
  //    the one label so `placeWireLabels` can ride whichever part of the net is on
  //    screen (orientation + keep-in-view), not just the first wire we saw.
  labelItems = [];
  const labelByText = new Map<string, { el: SVGTextElement; segs: [Pt, Pt][] }>();
  // The laid-out ELK edges keep their `e<schId>` ids, so map back to the model
  // edge for the net's canonical path (clicking a wire cross-probes that net).
  const edgeById = new Map(graph.edges.map((se) => [se.id, se]));
  // Bundle trunks (#117): the member taps of a raw access port were collapsed
  // to one ELK edge (keyed by the first member's id); the trunk cross-probes
  // the bundle itself, and the members re-fan at the consumer wall below.
  const trunkByRep = new Map<number, TrunkGroup>();
  for (const tg of trunkGroups(graph)) trunkByRep.set(tg.edges[0].id, tg);
  // Left-click a wire: highlight the net only — a plain click selects/cross-probes
  // but never navigates away (#147). Jumping to source is the explicit right-click
  // "Show in source" action. Right-click: highlight + open the action menu (append
  // to waveform / show in source).
  // Shared by the routed edges and the trunk fan-out geometry below. A member
  // stub passes its trunk's bundle path so selecting the member also lights
  // the trunk it hangs off (but never its sibling stubs).
  const wireHandlers = (netPath: string | undefined, trunkPath?: string) =>
    netPath
      ? {
          left: (ev: Event) => {
            ev.preventDefault();
            selectWire(netPath, trunkPath);
          },
          menu: (ev: MouseEvent) => {
            ev.preventDefault();
            selectWire(netPath, trunkPath);
            crossProbePath(netPath, ev);
          },
        }
      : null;
  for (const e of laid.edges ?? []) {
    const sch = edgeById.get(Number(String(e.id).slice(1)));
    const trunk = sch ? trunkByRep.get(sch.id) : undefined;
    const netPath = trunk ? trunk.path || undefined : sch?.net_path;
    const handlers = wireHandlers(netPath);
    const wireLeft = handlers?.left ?? null;
    const wireMenu = handlers?.menu ?? null;
    const segs: [Pt, Pt][] = [];
    for (const sec of e.sections ?? []) {
      const pts = [sec.startPoint, ...(sec.bendPoints ?? []), sec.endPoint];
      const points = pts.map((p: any) => `${p.x},${p.y}`).join(" ");
      const path = document.createElementNS(SVGNS, "polyline");
      path.setAttribute("class", netPath ? "wire clickable" : "wire");
      path.setAttribute("points", points);
      if (netPath) path.dataset.netPath = netPath;
      root.appendChild(path);
      // A wire is a 1.5px line; lay a transparent fat hit-line over it so the net
      // is comfortably clickable. Boxes are drawn after wires, so a box still wins
      // where a wire passes under it.
      if (wireLeft) {
        const hit = document.createElementNS(SVGNS, "polyline");
        hit.setAttribute("class", "wire-hit");
        hit.setAttribute("points", points);
        hit.onclick = wireLeft;
        hit.oncontextmenu = wireMenu;
        root.appendChild(hit);
      }
      for (let i = 0; i < pts.length - 1; i++) segs.push([pts[i], pts[i + 1]]);
    }
    const text = e.labels?.[0]?.text;
    if (text && segs.length) {
      let item = labelByText.get(text);
      if (!item) {
        const t = document.createElementNS(SVGNS, "text");
        t.setAttribute("class", "wire-label");
        t.setAttribute("text-anchor", "middle");
        t.textContent = text;
        // The label cross-probes the same net as its wire.
        if (wireLeft && netPath) {
          t.classList.add("clickable");
          t.dataset.netPath = netPath;
          t.onclick = wireLeft;
          t.oncontextmenu = wireMenu;
        }
        item = { el: t, segs: [] };
        labelByText.set(text, item);
        labelItems.push(item); // position + rotation set by placeWireLabels
      }
      // Accumulate this wire's segments so the single label can follow any part
      // of the net's fan-out into view.
      item.segs.push(...segs);
    }
  }

  // 1b. Bundle trunk fan-outs (#117): each collapsed member tap gets a short
  // stub from its pin to a gather bar one stub-length off the consumer wall —
  // one wire reaches the wall (the trunk, routed by ELK to the representative
  // pin, crossing the bar on its final approach), the split happens there.
  // Stubs keep per-member cross-probing; the bar belongs to the bundle.
  const drawWire = (pts: Pt[], netPath?: string, trunkPath?: string) => {
    const points = pts.map((p) => `${p.x},${p.y}`).join(" ");
    const path = document.createElementNS(SVGNS, "polyline");
    path.setAttribute("class", netPath ? "wire clickable" : "wire");
    path.setAttribute("points", points);
    if (netPath) path.dataset.netPath = netPath;
    if (trunkPath) path.dataset.trunkPath = trunkPath;
    root.appendChild(path);
    const handlers = wireHandlers(netPath, trunkPath);
    if (handlers) {
      const hit = document.createElementNS(SVGNS, "polyline");
      hit.setAttribute("class", "wire-hit");
      hit.setAttribute("points", points);
      hit.onclick = handlers.left;
      hit.oncontextmenu = handlers.menu;
      root.appendChild(hit);
    }
  };
  for (const tg of trunkByRep.values()) {
    const box = (laid.children ?? []).find((c: any) => c.id === nodeId(tg.box));
    if (!box) continue;
    const dir: 1 | -1 = tg.side === "east" ? 1 : -1;
    const members: { pt: Pt; e: (typeof tg.edges)[number] }[] = [];
    for (const m of tg.edges) {
      const pid = m.source === tg.port ? m.target : m.source;
      const p = (box.ports ?? []).find((q: any) => q.id === portId(pid));
      if (p) members.push({ pt: { x: (box.x ?? 0) + (p.x ?? 0), y: (box.y ?? 0) + (p.y ?? 0) }, e: m });
    }
    if (!members.length) continue;
    const geo = gatherBar(
      members.map((m) => m.pt),
      dir,
    );
    drawWire(geo.bar, tg.path || undefined);
    members.forEach((m, i) => drawWire(geo.stubs[i], m.e.net_path, tg.path || undefined));
  }

  // 2. Boxes.
  for (const c of laid.children ?? []) {
    const id = Number(String(c.id).slice(1));
    const node = graph.nodes.find((n) => n.id === id);

    // Boundary I/O pin (the scope's own port): a frame pin + label, not a box.
    if (node?.kind === "Port") {
      renderBoundaryPin(root, c, node, id);
      continue;
    }
    // Inferred storage (register FF / level latch): an FF-style symbol with
    // labelled west input rows.
    if (node?.kind === "FF" || node?.kind === "Latch") {
      renderStorage(root, c, node, id);
      continue;
    }
    // Continuous assign: a small anonymous square function node (#135).
    if (node?.kind === "Assign") {
      renderAssign(root, c, id);
      continue;
    }
    // Memory array (#112): an array-stack glyph with addr/din/dout pins.
    if (node?.kind === "Memory") {
      renderMemory(root, c, node, id);
      continue;
    }
    // SystemVerilog interface: a modport-qualified port draws as a square
    // frame pin (#125); an interface *instance* keeps the hexagon bundle box.
    if (node?.kind === "Interface") {
      if (node.modport) renderBundlePin(root, c, node, id);
      else renderInterface(root, c, node, id);
      continue;
    }

    const portById = new Map<number, SchPort>();
    node?.ports.forEach((p) => portById.set(p.id, p));

    const g = document.createElementNS(SVGNS, "g");
    g.setAttribute("transform", `translate(${c.x},${c.y})`);

    const rect = document.createElementNS(SVGNS, "rect");
    // Logic kinds get a per-kind class so combinational processes read apart
    // from module-instance boxes by colour. (Only Comb still reaches this
    // generic path — FF/Latch draw the storage symbol and Assign its capsule.)
    const kindClass = node && isLogicKind(node.kind) ? " " + node.kind.toLowerCase() : "";
    rect.setAttribute("class", "box" + kindClass + (state.selected === id ? " sel" : ""));
    rect.setAttribute("width", String(c.width));
    rect.setAttribute("height", String(c.height));
    rect.setAttribute("rx", "4");
    rect.dataset.nodeId = String(id);
    rect.onclick = () => selectNode(id);
    rect.ondblclick = () => {
      if (node?.expandable) {
        rememberCurrentView();
        setScope(node.path ?? "", node.label);
      }
    };
    rect.oncontextmenu = (e) => {
      e.preventDefault();
      crossProbe(id, e);
    };
    g.appendChild(rect);

    // Title: instance name, with the module type on a second line (like a
    // schematic block caption), centred in the box.
    const cx = c.width / 2;
    const cy = c.height / 2;
    const name = document.createElementNS(SVGNS, "text");
    name.setAttribute("class", "box-label" + (state.selected === id ? " sel" : ""));
    name.setAttribute("x", String(cx));
    name.setAttribute("y", String(node?.module ? cy - 4 : cy + 4));
    name.setAttribute("text-anchor", "middle");
    name.textContent = (c.labels?.[0]?.text ?? "") + (node?.expandable ? " ▸" : "");
    name.dataset.nodeId = String(id);
    name.style.pointerEvents = "none";
    g.appendChild(name);
    if (node?.module) {
      const mod = document.createElementNS(SVGNS, "text");
      mod.setAttribute("class", "box-sublabel" + (state.selected === id ? " sel" : ""));
      mod.setAttribute("x", String(cx));
      mod.setAttribute("y", String(cy + 12));
      mod.setAttribute("text-anchor", "middle");
      mod.textContent = `(${node.module})`;
      mod.dataset.nodeId = String(id);
      mod.style.pointerEvents = "none";
      g.appendChild(mod);
    }

    // Pins: a direction arrow bounded by the module — its base flush on the box
    // wall and apex pointing inward, so the pin is contained by the rectangle and
    // the wire lands exactly on the boundary (in on the west, out on the east).
    // The perpendicular position is anchored to the box edge (not ELK's port x)
    // so pins stay inside regardless of ELK's port-offset convention.
    const PIN = 8; // triangle depth
    const LABEL_PAD = 11; // gap from the wall to the pin label
    for (const p of c.ports ?? []) {
      const pid = Number(String(p.id).slice(1));
      const sp = portById.get(pid);
      const py = p.y ?? 0;
      const west = sp ? sp.side !== "east" : (p.x ?? 0) < c.width / 2;
      const edgeX = west ? 0 : c.width;

      const arrow = document.createElementNS(SVGNS, "path");
      arrow.setAttribute(
        "class",
        "pin " + (west ? "pin-in" : "pin-out") + (sp?.dangling ? " dangling" : ""),
      );
      // A bundle pin (whole-interface connection) is a square; a normal pin a
      // directional triangle.
      arrow.setAttribute(
        "d",
        sp?.bundle
          ? `M${west ? edgeX : edgeX - PIN},${py - 4} h${PIN} v8 h${-PIN} Z`
          : west
            ? `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX + PIN},${py} Z`
            : `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX - PIN},${py} Z`,
      );
      // A pin selects + highlights (left) and cross-probes to source + waveform
      // (right) by its own model path; if the pin has no path, fall back to the
      // containing box so right-click is never a dead gesture.
      const probePin = (e: MouseEvent) => {
        e.preventDefault();
        if (sp?.path) crossProbePath(sp.path, e);
        else crossProbe(id, e);
      };
      arrow.dataset.nodeId = String(pid);
      arrow.onclick = () => selectNode(pid);
      arrow.oncontextmenu = probePin;
      g.appendChild(arrow);

      // A logic node (comb/latch/assign) is a process, not a module: its pins are
      // bare wire stubs, so skip the per-pin signal-name labels (the wire already
      // carries that name).
      if (sp && !isLogicKind(node?.kind ?? "")) {
        const t = document.createElementNS(SVGNS, "text");
        t.setAttribute("class", "pin-label" + (sp.dangling ? " dangling" : ""));
        t.setAttribute("x", String(west ? edgeX + LABEL_PAD : edgeX - LABEL_PAD));
        t.setAttribute("y", String(py + 3));
        t.setAttribute("text-anchor", west ? "start" : "end");
        t.textContent = sp.width ? `${sp.name}${sp.width}` : sp.name;
        t.dataset.nodeId = String(pid);
        t.onclick = () => selectNode(pid);
        t.oncontextmenu = probePin;
        g.appendChild(t);
      }
    }
    root.appendChild(g);
  }

  // 3. Net labels last, so they stay legible over wires and box edges.
  for (const it of labelItems) root.appendChild(it.el);

  // A view change (drill-in / breadcrumb-jump): zoom-to-fit so the whole scope
  // is visible, scrolled top-left — unless we're navigating back, in which case
  // restore the viewport we left. (Manual zoom via setZoom is unaffected after.)
  zoom.k = restore ? restore.k : fitZoom(baseW, baseH, host.clientWidth, host.clientHeight);
  host.appendChild(svg);
  applyZoom(svg);
  host.scrollLeft = restore ? restore.scrollLeft : 0;
  host.scrollTop = restore ? restore.scrollTop : 0;
  // Place labels against the final viewport (orientation + visible-portion).
  placeWireLabels();
  // Drawn while the schematic tab is hidden (0-width host → a degenerate fit):
  // defer the real fit until the tab is next shown (#99).
  schematicDirty = host.clientWidth === 0;
}

// Position each net label on the currently-visible portion of its wire, rotated
// to run along a vertical segment. Picks the longest segment in view (so the
// label rides the on-screen part of the wire — #28); falls back to the longest
// segment overall when the wire is fully off-screen. Cheap; safe to call on every
// pan/zoom.
function placeWireLabels() {
  if (!labelItems.length) return;
  const host = $("schematic");
  const k = zoom.k || 1;
  // Visible region in base (pre-scale) coordinates.
  const view = {
    x0: host.scrollLeft / k,
    y0: host.scrollTop / k,
    x1: (host.scrollLeft + host.clientWidth) / k,
    y1: (host.scrollTop + host.clientHeight) / k,
  };
  const manhattan = (a: Pt, b: Pt) => Math.abs(a.x - b.x) + Math.abs(a.y - b.y);
  for (const { el, segs } of labelItems) {
    let best: [Pt, Pt] | null = null;
    let bestLen = -1;
    for (const [a, b] of segs) {
      const vis = clampSegmentToRect(a, b, view);
      if (!vis) continue;
      const len = manhattan(vis[0], vis[1]);
      if (len > bestLen) [bestLen, best] = [len, vis];
    }
    // Fully off-screen: keep a stable home on the longest segment overall.
    if (!best) {
      for (const [a, b] of segs) {
        const len = manhattan(a, b);
        if (len > bestLen) [bestLen, best] = [len, [a, b]];
      }
    }
    if (!best) continue;
    const p = wireLabelPlacement(best[0], best[1]);
    el.setAttribute("x", String(p.x));
    el.setAttribute("y", String(p.y));
    el.setAttribute("text-anchor", p.anchor);
    el.setAttribute("dominant-baseline", p.baseline);
    if (p.rotate) el.setAttribute("transform", `rotate(${p.rotate} ${p.x} ${p.y})`);
    else el.removeAttribute("transform");
  }
}

// A scope's own port, drawn as a frame pin: an arrow along the signal flow plus
// the port name on the outboard side (inputs on the left, outputs on the right).
function renderBoundaryPin(parent: SVGElement, c: any, node: SchNode, id: number) {
  const sp = node.ports[0];
  const p = c.ports?.[0];
  const px = p?.x ?? 0;
  const py = p?.y ?? 0;
  const input = sp?.side === "east"; // east-facing pin ⇒ input on the west frame
  // A constant tie-off is a synthetic node (no model id) — render its literal and
  // make it inert; a real boundary I/O pin cross-probes on click.
  const isConst = !!node.constant;
  const g = document.createElementNS(SVGNS, "g");
  g.setAttribute("transform", `translate(${c.x},${c.y})`);

  const arrow = document.createElementNS(SVGNS, "path");
  arrow.setAttribute("class", "pin " + (input ? "pin-in" : "pin-out"));
  arrow.setAttribute(
    "d",
    input
      ? `M${px - 8},${py - 4} L${px - 8},${py + 4} L${px},${py} Z`
      : `M${px + 8},${py - 4} L${px + 8},${py + 4} L${px},${py} Z`,
  );
  // A real boundary I/O pin selects (left) and cross-probes to source + waveform
  // (right), like a box; a constant tie-off is inert.
  const probePin = (ev: MouseEvent) => {
    ev.preventDefault();
    crossProbe(id, ev);
  };
  if (!isConst) {
    arrow.onclick = () => selectNode(id);
    arrow.oncontextmenu = probePin;
  }
  g.appendChild(arrow);

  const t = document.createElementNS(SVGNS, "text");
  t.setAttribute("class", isConst ? "const-label" : "pin-label");
  t.setAttribute("x", String(input ? px - 12 : px + 12));
  t.setAttribute("y", String(py + 3));
  t.setAttribute("text-anchor", input ? "end" : "start");
  t.textContent = sp?.width ? `${sp.name}${sp.width}` : (sp?.name ?? node.label);
  if (!isConst) {
    t.onclick = () => selectNode(id);
    t.oncontextmenu = probePin;
  }
  g.appendChild(t);

  parent.appendChild(g);
}

// A modport-qualified interface port (#125): the scope's window to the outside,
// drawn as a square frame pin — the "interface port" glyph (#106/#96) — rather
// than the hexagon reserved for interface *instances*. It shares the frame
// column with the scope's own boundary pins (walls flush), and every wired
// member wire anchors at the square (FIXED_POS ports at the wall centre) before
// fanning out toward the design; the pin carries the instance name with the
// `(type.view)` sublabel outboard.
function renderBundlePin(parent: SVGElement, c: any, node: SchNode, id: number) {
  const W = c.width;
  const H = c.height;
  // Same mostly-in/mostly-out test as toElk: a FIRST-layer pin faces the
  // design with its east wall; a LAST-layer pin with its west wall.
  const eastCount = node.ports.filter((p) => p.side === "east").length;
  const first = eastCount >= node.ports.length - eastCount;
  const edgeX = first ? W : 0;
  const cy = H / 2;

  const g = document.createElementNS(SVGNS, "g");
  g.setAttribute("transform", `translate(${c.x},${c.y})`);

  const SQ = 5; // square half-size, straddling the wall like a frame pin
  const square = document.createElementNS(SVGNS, "path");
  square.setAttribute("class", "pin bundle-pin" + (state.selected === id ? " sel" : ""));
  square.setAttribute("d", `M${edgeX - SQ},${cy - SQ} h${2 * SQ} v${2 * SQ} h${-2 * SQ} Z`);
  square.dataset.nodeId = String(id);
  const probePin = (ev: MouseEvent) => {
    ev.preventDefault();
    crossProbe(id, ev);
  };
  square.onclick = () => selectNode(id);
  square.oncontextmenu = probePin;
  g.appendChild(square);

  // Instance name first, `(type.view)` on the grey sublabel line — the same
  // reading order as the hexagon, anchored away from the design side.
  const lx = first ? edgeX - SQ - 6 : edgeX + SQ + 6;
  const anchor = first ? "end" : "start";
  const name = document.createElementNS(SVGNS, "text");
  name.setAttribute("class", "pin-label" + (state.selected === id ? " sel" : ""));
  name.setAttribute("x", String(lx));
  name.setAttribute("y", String(cy - 2));
  name.setAttribute("text-anchor", anchor);
  name.textContent = node.label;
  name.dataset.nodeId = String(id);
  name.onclick = () => selectNode(id);
  name.oncontextmenu = probePin;
  g.appendChild(name);
  if (node.module) {
    const mod = document.createElementNS(SVGNS, "text");
    mod.setAttribute("class", "box-sublabel" + (state.selected === id ? " sel" : ""));
    mod.setAttribute("x", String(lx));
    mod.setAttribute("y", String(cy + 10));
    mod.setAttribute("text-anchor", anchor);
    mod.textContent = `(${node.module}.${node.modport})`;
    mod.dataset.nodeId = String(id);
    mod.onclick = () => selectNode(id);
    mod.oncontextmenu = probePin;
    g.appendChild(mod);
  }

  parent.appendChild(g);
}

// An inferred storage element — register FF or level latch (#115): a box
// captioned "FF"/"LE" in its bottom band, the data + enable inputs as labelled
// rows down the west wall (enable in its own colour), a clock wedge (clk, left
// wall, low), an active-low bubble (async reset, outer bottom centre), and one
// Q stub per distinct output on the east wall. Pin positions come from ELK
// (FIXED_POS in `storageChild`); here we add the glyphs and labels.
function renderStorage(parent: SVGElement, c: any, node: SchNode, id: number) {
  const W = c.width;
  const H = c.height;
  const g = document.createElementNS(SVGNS, "g");
  g.setAttribute("transform", `translate(${c.x},${c.y})`);

  const rect = document.createElementNS(SVGNS, "rect");
  const kindClass = node.kind === "Latch" ? "latch" : "ff";
  rect.setAttribute("class", `box ${kindClass}` + (state.selected === id ? " sel" : ""));
  rect.setAttribute("width", String(W));
  rect.setAttribute("height", String(H));
  rect.setAttribute("rx", "3");
  rect.dataset.nodeId = String(id);
  rect.onclick = () => selectNode(id);
  rect.oncontextmenu = (e) => {
    e.preventDefault();
    crossProbe(id, e);
  };
  g.appendChild(rect);

  const t = document.createElementNS(SVGNS, "text");
  t.setAttribute("class", "box-label" + (state.selected === id ? " sel" : ""));
  t.setAttribute("x", String(W / 2));
  t.setAttribute("y", String(H / 2 + 1));
  t.setAttribute("text-anchor", "middle");
  t.dataset.nodeId = String(id);
  t.style.pointerEvents = "none";
  t.textContent = c.labels?.[0]?.text ?? "FF";
  g.appendChild(t);

  const portById = new Map<number, SchPort>();
  node.ports.forEach((p) => portById.set(p.id, p));
  for (const p of c.ports ?? []) {
    const pid = Number(String(p.id).slice(1));
    const sp = portById.get(pid);
    if (!sp) continue;
    const px = p.x ?? 0;
    const py = p.y ?? 0;
    // A pin selects + highlights (left) and cross-probes (right) by its own
    // model path, falling back to the box so right-click is never dead.
    const probePin = (e: MouseEvent) => {
      e.preventDefault();
      if (sp.path) crossProbePath(sp.path, e);
      else crossProbe(id, e);
    };
    const wirePin = (el: SVGElement) => {
      el.dataset.nodeId = String(pid);
      el.onclick = () => selectNode(pid);
      el.oncontextmenu = probePin;
      g.appendChild(el);
    };
    const role = ffRole(sp, node.kind);
    if (role === "clk") {
      // Clock-edge wedge on the left wall: base flush to the wall, apex pointing
      // right into the box.
      const tri = document.createElementNS(SVGNS, "path");
      tri.setAttribute("class", "ff-clk");
      tri.setAttribute("d", `M0,${py - 6} L0,${py + 6} L10,${py} Z`);
      wirePin(tri);
    } else if (role === "reset") {
      // Active-low reset bubble, centred on and just below the bottom edge.
      const circ = document.createElementNS(SVGNS, "circle");
      circ.setAttribute("class", "ff-rst");
      circ.setAttribute("cx", String(px));
      circ.setAttribute("cy", String(H + 3));
      circ.setAttribute("r", "3");
      wirePin(circ);
    } else if (role === "q") {
      // One output stub per distinct output, so a register driving several
      // signals shows each output individually (base on the east wall, apex in).
      // A wired Q carries no label (the wire label names the net); a dangling
      // Q (#118) is labelled in-box, dimmed, since no wire exists to name it.
      const tri = document.createElementNS(SVGNS, "path");
      tri.setAttribute("class", "pin pin-out" + (sp.dangling ? " dangling" : ""));
      tri.setAttribute("d", `M${W},${py - 4} L${W},${py + 4} L${W - 8},${py} Z`);
      wirePin(tri);
      if (sp.dangling) {
        const lab = document.createElementNS(SVGNS, "text");
        lab.setAttribute("class", "pin-label dangling");
        lab.setAttribute("x", String(W - FF_LABEL_PAD));
        lab.setAttribute("y", String(py + 3));
        lab.setAttribute("text-anchor", "end");
        lab.textContent = sp.width ? `${sp.name}${sp.width}` : sp.name;
        wirePin(lab);
      }
    } else {
      // Data/enable input row on the west wall: a stub (base flush, apex in)
      // plus the signal-name label, so the glyph says which input is which.
      const tri = document.createElementNS(SVGNS, "path");
      tri.setAttribute("class", "pin " + (role === "enable" ? "pin-en" : "pin-in"));
      tri.setAttribute("d", `M0,${py - 4} L0,${py + 4} L8,${py} Z`);
      wirePin(tri);
      const lab = document.createElementNS(SVGNS, "text");
      lab.setAttribute("class", "pin-label");
      lab.setAttribute("x", String(FF_LABEL_PAD));
      lab.setAttribute("y", String(py + 3));
      lab.setAttribute("text-anchor", "start");
      lab.textContent = sp.width ? `${sp.name}${sp.width}` : sp.name;
      wirePin(lab);
    }
  }
  parent.appendChild(g);
}

// A SystemVerilog interface — a signal bundle, drawn as a box with a folded
// top-right corner (a "dog-ear") so it reads as a bundle rather than a module
// instance. Carries its interface type as a sublabel (e.g. `(mem_if)`) and any
// interface ports (e.g. `clk`) as pins. Single-click selects, right-click
// cross-probes; a bundle with modport views is drillable (#97) — double-click
// descends into its modports/members (the caret ▸ marks it).
function renderInterface(parent: SVGElement, c: any, node: SchNode, id: number) {
  const W = c.width;
  const H = c.height;
  const portById = new Map<number, SchPort>();
  node.ports.forEach((p) => portById.set(p.id, p));

  const g = document.createElementNS(SVGNS, "g");
  g.setAttribute("transform", `translate(${c.x},${c.y})`);

  // Bundle body: a pointed hexagon (apex caps top and bottom, straight side
  // walls for the pins) — matches the reference bundle glyph. Pin rows stay on
  // the walls because the layout pads the height by IFACE_CAP.
  const body = document.createElementNS(SVGNS, "path");
  body.setAttribute("class", "box iface" + (state.selected === id ? " sel" : ""));
  body.setAttribute(
    "d",
    `M${W / 2},0 L${W},${IFACE_CAP} L${W},${H - IFACE_CAP} L${W / 2},${H} ` +
      `L0,${H - IFACE_CAP} L0,${IFACE_CAP} Z`,
  );
  body.dataset.nodeId = String(id);
  body.onclick = () => selectNode(id);
  body.ondblclick = () => {
    if (node.expandable) {
      rememberCurrentView();
      setScope(node.path ?? "", node.label);
    }
  };
  body.oncontextmenu = (e) => {
    e.preventDefault();
    crossProbe(id, e);
  };
  g.appendChild(body);

  // Instance name reads first — same convention as module boxes and the
  // reference snapshots — with the interface type on the grey sublabel line.
  // (A modport-qualified port never reaches here — it draws as a square frame
  // pin instead, #125.)
  const cx = W / 2;
  const cy = H / 2;
  const inst = c.labels?.[0]?.text ?? node.label;
  const type_ = node.module ?? null;
  const name = document.createElementNS(SVGNS, "text");
  name.setAttribute("class", "box-label" + (state.selected === id ? " sel" : ""));
  name.setAttribute("x", String(cx));
  name.setAttribute("y", String(type_ ? cy - 4 : cy + 4));
  name.setAttribute("text-anchor", "middle");
  name.textContent = inst + (node.expandable ? " ▸" : "");
  name.dataset.nodeId = String(id);
  name.style.pointerEvents = "none";
  g.appendChild(name);
  if (type_) {
    const mod = document.createElementNS(SVGNS, "text");
    mod.setAttribute("class", "box-sublabel" + (state.selected === id ? " sel" : ""));
    mod.setAttribute("x", String(cx));
    mod.setAttribute("y", String(cy + 12));
    mod.setAttribute("text-anchor", "middle");
    mod.textContent = type_;
    mod.dataset.nodeId = String(id);
    mod.style.pointerEvents = "none";
    g.appendChild(mod);
  }

  // Interface ports: real ports (e.g. clk) plus the aggregate access ports
  // (#96 — one per consuming modport view, one raw fan-out port), drawn like
  // a module box's pins.
  const PIN = 8;
  const LABEL_PAD = 11;
  for (const p of c.ports ?? []) {
    const pid = Number(String(p.id).slice(1));
    const sp = portById.get(pid);
    const py = p.y ?? 0;
    const west = sp ? sp.side !== "east" : (p.x ?? 0) < W / 2;
    const edgeX = west ? 0 : W;
    const arrow = document.createElementNS(SVGNS, "path");
    arrow.setAttribute(
      "class",
      "pin " + (west ? "pin-in" : "pin-out") + (sp?.dangling ? " dangling" : ""),
    );
    // Aggregate access ports (#96) are bundle pins — squares, unlike the
    // directional triangle of a normal pin (e.g. clk).
    arrow.setAttribute(
      "d",
      sp?.bundle
        ? `M${west ? edgeX : edgeX - PIN},${py - 4} h${PIN} v8 h${-PIN} Z`
        : west
          ? `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX + PIN},${py} Z`
          : `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX - PIN},${py} Z`,
    );
    const probePin = (e: MouseEvent) => {
      e.preventDefault();
      if (sp?.path) crossProbePath(sp.path, e);
      else crossProbe(id, e);
    };
    arrow.dataset.nodeId = String(pid);
    arrow.onclick = () => selectNode(pid);
    arrow.oncontextmenu = probePin;
    g.appendChild(arrow);
    if (sp) {
      const t = document.createElementNS(SVGNS, "text");
      t.setAttribute("class", "pin-label");
      t.setAttribute("x", String(west ? edgeX + LABEL_PAD : edgeX - LABEL_PAD));
      t.setAttribute("y", String(py + 3));
      t.setAttribute("text-anchor", west ? "start" : "end");
      t.textContent = sp.width ? `${sp.name}${sp.width}` : sp.name;
      t.dataset.nodeId = String(pid);
      t.onclick = () => selectNode(pid);
      t.oncontextmenu = probePin;
      g.appendChild(t);
    }
  }
  parent.appendChild(g);
}

// A continuous assign: a small anonymous square (#135) — no text label, no pin
// glyphs (at 16 px they would outsize the box). The wire net labels carry the
// meaning; the shape keeps selection and click/right-click cross-probe.
function renderAssign(parent: SVGElement, c: any, id: number) {
  const g = document.createElementNS(SVGNS, "g");
  g.setAttribute("transform", `translate(${c.x},${c.y})`);

  const rect = document.createElementNS(SVGNS, "rect");
  rect.setAttribute("class", "box assign" + (state.selected === id ? " sel" : ""));
  rect.setAttribute("width", String(c.width));
  rect.setAttribute("height", String(c.height));
  rect.setAttribute("rx", "2");
  rect.setAttribute("ry", "2");
  rect.dataset.nodeId = String(id);
  rect.onclick = () => selectNode(id);
  rect.oncontextmenu = (e) => {
    e.preventDefault();
    crossProbe(id, e);
  };
  const tip = document.createElementNS(SVGNS, "title");
  tip.textContent = "assign";
  rect.appendChild(tip);
  g.appendChild(rect);
  parent.appendChild(g);
}

// Element bit-width of a memory from any of its data pins (`[31:0]` -> 32), for
// the "depth×width" sublabel. `null` if no pin carries a width.
function memWidth(node: SchNode): number | null {
  for (const p of node.ports) {
    const m = p.width?.match(/\[(\d+):(\d+)\]/);
    if (m) return Math.abs(Number(m[1]) - Number(m[2])) + 1;
  }
  return null;
}

// A memory array (#112): an array-stack box — offset back-cards and word-row
// dividers so it reads as a RAM, not a plain box or an FF — with addr/din inputs
// labelled down the west wall, dout output(s) on the east, a depth×width
// sublabel, and an INIT badge (tooltip = the $readmemh source) when the array is
// initialized. Each pin cross-probes its own signal path; the box its node.
function renderMemory(parent: SVGElement, c: any, node: SchNode, id: number) {
  const W = c.width;
  const H = c.height;
  const g = document.createElementNS(SVGNS, "g");
  g.setAttribute("transform", `translate(${c.x},${c.y})`);
  const selCls = state.selected === id ? " sel" : "";

  // Stacked back-cards (offset up-right) behind the body, so the box reads as a
  // stack of words — the array motif. Decorative: no pointer events.
  for (const off of [6, 3]) {
    const back = document.createElementNS(SVGNS, "rect");
    back.setAttribute("class", "box memory mem-stack" + selCls);
    back.setAttribute("x", String(off));
    back.setAttribute("y", String(-off));
    back.setAttribute("width", String(W));
    back.setAttribute("height", String(H));
    back.setAttribute("rx", "3");
    back.style.pointerEvents = "none";
    g.appendChild(back);
  }

  const rect = document.createElementNS(SVGNS, "rect");
  rect.setAttribute("class", "box memory" + selCls);
  rect.setAttribute("width", String(W));
  rect.setAttribute("height", String(H));
  rect.setAttribute("rx", "3");
  rect.dataset.nodeId = String(id);
  rect.onclick = () => selectNode(id);
  rect.oncontextmenu = (e) => {
    e.preventDefault();
    crossProbe(id, e);
  };
  g.appendChild(rect);

  // Word-row dividers in the lower band, reinforcing the array look.
  for (const fy of [0.62, 0.79]) {
    const line = document.createElementNS(SVGNS, "line");
    line.setAttribute("class", "mem-row");
    line.setAttribute("x1", "0");
    line.setAttribute("x2", String(W));
    line.setAttribute("y1", String(Math.round(H * fy)));
    line.setAttribute("y2", String(Math.round(H * fy)));
    line.style.pointerEvents = "none";
    g.appendChild(line);
  }

  // Title: array name, then a depth×width (or [0:N]) sublabel.
  const t = document.createElementNS(SVGNS, "text");
  t.setAttribute("class", "box-label" + selCls);
  t.setAttribute("x", String(W / 2));
  t.setAttribute("y", "12");
  t.setAttribute("text-anchor", "middle");
  t.style.pointerEvents = "none";
  t.textContent = node.label;
  g.appendChild(t);

  if (node.memDepth != null) {
    const bits = memWidth(node);
    const s = document.createElementNS(SVGNS, "text");
    s.setAttribute("class", "box-sublabel" + selCls);
    s.setAttribute("x", String(W / 2));
    s.setAttribute("y", "24");
    s.setAttribute("text-anchor", "middle");
    s.style.pointerEvents = "none";
    s.textContent = bits ? `${node.memDepth}×${bits}` : `[0:${node.memDepth - 1}]`;
    g.appendChild(s);
  }

  // INIT badge (top-right) when the array is $readmemh-initialized.
  if (node.initSource) {
    const badge = document.createElementNS(SVGNS, "g");
    badge.style.pointerEvents = "none";
    const br = document.createElementNS(SVGNS, "rect");
    br.setAttribute("class", "mem-init");
    br.setAttribute("x", String(W - 30));
    br.setAttribute("y", "-8");
    br.setAttribute("width", "28");
    br.setAttribute("height", "12");
    br.setAttribute("rx", "2");
    badge.appendChild(br);
    const bt = document.createElementNS(SVGNS, "text");
    bt.setAttribute("class", "mem-init-label");
    bt.setAttribute("x", String(W - 16));
    bt.setAttribute("y", "1");
    bt.setAttribute("text-anchor", "middle");
    bt.textContent = "INIT";
    badge.appendChild(bt);
    const tip = document.createElementNS(SVGNS, "title");
    tip.textContent = `$readmemh(${node.initSource})`;
    badge.appendChild(tip);
    g.appendChild(badge);
  }

  // Pins: addr/din inputs (west) and dout output(s) (east), labelled by role.
  const portById = new Map<number, SchPort>();
  node.ports.forEach((p) => portById.set(p.id, p));
  for (const p of c.ports ?? []) {
    const pid = Number(String(p.id).slice(1));
    const sp = portById.get(pid);
    if (!sp) continue;
    const py = p.y ?? 0;
    const east = sp.side === "east";
    const probePin = (e: MouseEvent) => {
      e.preventDefault();
      if (sp.path) crossProbePath(sp.path, e);
      else crossProbe(id, e);
    };
    const wirePin = (el: SVGElement) => {
      el.dataset.nodeId = String(pid);
      el.onclick = () => selectNode(pid);
      el.oncontextmenu = probePin;
      g.appendChild(el);
    };
    const tri = document.createElementNS(SVGNS, "path");
    tri.setAttribute("class", "pin " + (east ? "pin-out" : "pin-in"));
    tri.setAttribute(
      "d",
      east ? `M${W},${py - 4} L${W},${py + 4} L${W - 8},${py} Z` : `M0,${py - 4} L0,${py + 4} L8,${py} Z`,
    );
    wirePin(tri);
    const lab = document.createElementNS(SVGNS, "text");
    lab.setAttribute("class", "pin-label");
    lab.setAttribute("x", String(east ? W - MEM_LABEL_PAD : MEM_LABEL_PAD));
    lab.setAttribute("y", String(py + 3));
    lab.setAttribute("text-anchor", east ? "end" : "start");
    lab.textContent = sp.role ?? sp.name;
    wirePin(lab);
  }
  parent.appendChild(g);
}

// Resize the SVG and scale its content to the current zoom factor (no relayout).
function applyZoom(svg: SVGSVGElement) {
  const bw = Number(svg.dataset.baseW) || 400;
  const bh = Number(svg.dataset.baseH) || 300;
  svg.setAttribute("width", String(Math.round(bw * zoom.k)));
  svg.setAttribute("height", String(Math.round(bh * zoom.k)));
  (svg.firstElementChild as SVGGElement)?.setAttribute("transform", `scale(${zoom.k})`);
  const pct = $("zoom-reset");
  if (pct) pct.textContent = `${Math.round(zoom.k * 100)}%`;
}

// Zoom around a focal point (keeps that document point under the cursor).
function setZoom(k: number, focus?: { x: number; y: number }) {
  const host = $("schematic");
  const svg = host.querySelector("svg");
  if (!svg) return;
  const prev = zoom.k;
  zoom.k = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, k));
  const rect = host.getBoundingClientRect();
  const fx = focus ? focus.x - rect.left : host.clientWidth / 2;
  const fy = focus ? focus.y - rect.top : host.clientHeight / 2;
  const ox = host.scrollLeft + fx;
  const oy = host.scrollTop + fy;
  applyZoom(svg as SVGSVGElement);
  const ratio = zoom.k / prev;
  host.scrollLeft = ox * ratio - fx;
  host.scrollTop = oy * ratio - fy;
  placeWireLabels();
}

// Fit the whole current scope into the pane (zoom-to-fit, scrolled to origin) —
// the view the schematic opens at. Bound to Ctrl/⌘+0 and the zoom-reset button.
function fitView() {
  const host = $("schematic");
  const svg = host.querySelector("svg") as SVGSVGElement | null;
  if (!svg) return;
  const bw = Number(svg.dataset.baseW) || 400;
  const bh = Number(svg.dataset.baseH) || 300;
  zoom.k = fitZoom(bw, bh, host.clientWidth, host.clientHeight, MAX_ZOOM);
  applyZoom(svg);
  host.scrollLeft = 0;
  host.scrollTop = 0;
  placeWireLabels();
}

// -- tabs (#99) ------------------------------------------------------------

// Show the panel `panelId` within its .tab-group, hide its siblings, sync the tab
// buttons + per-tab aux controls, and redraw the now-visible view — its canvas/SVG
// had zero size while hidden, so it must re-fit/redraw against the real dimensions.
function activateTab(panelId: string) {
  const group = document.getElementById(panelId)?.closest(".tab-group");
  if (!group) return;
  group.querySelectorAll<HTMLElement>(".tab-panel").forEach((p) =>
    p.classList.toggle("active", p.id === panelId),
  );
  group.querySelectorAll<HTMLButtonElement>(".tab").forEach((b) => {
    const on = b.dataset.panel === panelId;
    b.classList.toggle("active", on);
    b.setAttribute("aria-selected", String(on));
    // On-demand tabs (schematic / waveform, #17) start hidden; activating one —
    // via the toolbar Show buttons or an append/show-in action — reveals it.
    if (on) b.hidden = false;
  });
  group.querySelectorAll<HTMLElement>(".tab-aux").forEach((a) => {
    a.hidden = a.dataset.panel !== panelId;
  });
  if (panelId === "schematic-pane") refreshSchematic();
  else if (panelId === "wave-pane") redrawTracks();
}

// Re-fit the schematic if it was last drawn while hidden (#99); otherwise just
// re-place the net labels against the now-visible viewport.
function refreshSchematic() {
  if (schematicDirty) {
    schematicDirty = false;
    fitView();
  } else {
    placeWireLabels();
  }
}

// Zoom affects the schematic SVG only — never the page/webview. Ctrl/⌘ + wheel
// and Ctrl/⌘ + (+/-/0) are intercepted at the document (capture, non-passive) so
// the browser/webview can't page-zoom the whole window; the gesture is routed to
// our SVG zoom (toward the cursor for the wheel). Plain wheel still scrolls.
function setupZoom() {
  const host = $("schematic");
  document.addEventListener(
    "wheel",
    (ev) => {
      if (!ev.ctrlKey && !ev.metaKey) return;
      ev.preventDefault(); // stop page/webview zoom everywhere
      if (host.contains(ev.target as Node)) {
        setZoom(zoom.k * (ev.deltaY < 0 ? 1.15 : 1 / 1.15), { x: ev.clientX, y: ev.clientY });
      }
    },
    { passive: false, capture: true },
  );
  document.addEventListener("keydown", (ev) => {
    if (!(ev.ctrlKey || ev.metaKey)) return;
    if (ev.key === "+" || ev.key === "=") setZoom(zoom.k * 1.25);
    else if (ev.key === "-" || ev.key === "_") setZoom(zoom.k / 1.25);
    else if (ev.key === "0") fitView(); // restore the fitted view
    else return;
    ev.preventDefault(); // stop the webview's own +/-/0 page zoom
  });
  $("zoom-in").addEventListener("click", () => setZoom(zoom.k * 1.25));
  $("zoom-out").addEventListener("click", () => setZoom(zoom.k / 1.25));
  $("zoom-reset").addEventListener("click", fitView); // fit, not actual-size 100%
  // Panning (native scroll) re-places net labels onto the visible wire portion.
  host.addEventListener(
    "scroll",
    () => {
      placeWireLabels();
    },
    { passive: true },
  );
}

// `node.path` isn't in the layout; look it up from the graph by id.
function pathOf(id: number): string | null {
  return state.graph?.nodes.find((n) => n.id === id)?.path ?? null;
}

// Single-click selection: highlight the node in the schematic only. This is
// deliberately synchronous and non-destructive — it moves the `.sel` class in
// place rather than re-rendering, so a following double-click still lands on the
// same element and drills reliably. (The old async re-render here wiped the SVG
// mid-double-click and destroyed the drill target → intermittent drilling, #47.)
// Source/waveform cross-probe no longer fires on single-click; it moves to
// right-click — see `crossProbe`.
function selectNode(id: number) {
  state.selected = id;
  applySelection();
}

// Reflect `state.selected` by moving the `.sel` class only, leaving the rest of
// the DOM intact so any pending double-click target survives. Boxes, pins and
// caption labels (box-label / box-sublabel, #121) all carry `data-node-id`, so
// they highlight together. Exactly one object is highlighted at a time: applying
// a node selection also drops any highlighted wire (and `selectWire` drops the
// node selection in return).
function applySelection() {
  const host = $("schematic");
  host
    .querySelectorAll(
      ".box.sel, .pin.sel, .pin-label.sel, .box-label.sel, .box-sublabel.sel, " +
        ".wire.sel, .wire-label.sel",
    )
    .forEach((el) => el.classList.remove("sel"));
  if (state.selected != null) {
    host
      .querySelectorAll(`[data-node-id="${state.selected}"]`)
      .forEach((el) => el.classList.add("sel"));
  }
}

// Right-click a box/pin to cross-probe it to source + waveform. A polished
// drop-down menu is the later-stage enhancement; this keeps cross-probing
// reachable now that single-click is schematic-only (#47).
async function crossProbe(id: number, ev: MouseEvent) {
  const path = pathOf(id);
  if (path) await crossProbePath(path, ev);
}

// Cross-probe a node by its canonical model path and open the action menu at the
// cursor. Used for wires (whose net carries a path, not a graph-node id) and, via
// `crossProbe`, for boxes/pins.
async function crossProbePath(path: string, ev: MouseEvent) {
  await schematicMenu(ev, path);
}

// Live "Append to ▸ <pane>" destinations (#170): the main window plus every open
// waveform pop-out, read from the window list so it works from any window.
async function waveformDestinations(): Promise<string[]> {
  try {
    const { WebviewWindow } = await import("@tauri-apps/api/webviewWindow");
    const labels = (await WebviewWindow.getAll())
      .map((w) => w.label)
      .filter((l) => l.startsWith("waveform-"))
      .sort();
    return ["main", ...labels];
  } catch {
    return ["main"];
  }
}

// The "Append to waveform" menu item for a resolved cross-probe (#170). With no
// pop-outs it appends to main's own waveform; with live panes it becomes a flyout,
// one entry per window, each addressing that window by label. Disabled when the
// origin has no trace signal (the destination re-resolves by model path on append).
async function appendWaveItem(resp: ProbeResponse): Promise<MenuItem> {
  const enabled = resp.wave.in_trace;
  const suffix = enabled ? "" : " (not in trace)";
  const to = (dest: string) =>
    void publish(crossProbeSelection(resp, ["waveform"], selfLabel, dest));
  const dests = await waveformDestinations();
  if (dests.length === 1) {
    return { label: `Append to waveform${suffix}`, enabled, onClick: () => to("main") };
  }
  const name = (d: string) => (d === "main" ? "main window" : d);
  return {
    label: `Append to waveform${suffix}`,
    enabled,
    submenu: dests.map((d) => ({ label: name(d), enabled, onClick: () => to(d) })),
  };
}

// Right-click action menu for a schematic object: resolve it once, then offer
// "Append to waveform" (when the object has a trace signal) and "Show in source"
// (when it has a source location). Disabled items annotate why.
async function schematicMenu(ev: MouseEvent, path: string) {
  let resp: ProbeResponse | null;
  try {
    resp = await api.probeNode(path, context());
  } catch (e) {
    log("error", `probe failed: ${e}`);
    return;
  }
  if (!resp) return;
  openContextMenu(ev.clientX, ev.clientY, [
    await appendWaveItem(resp),
    {
      label: resp.source ? "Show in source" : "Show in source (no location)",
      enabled: !!resp.source,
      onClick: () =>
        void publish(crossProbeSelection(resp, ["source"], selfLabel)),
    },
  ]);
}

// Highlight every wire + label carrying `netPath` (the net just clicked), and
// clear it from the rest — including any selected box/pin, so exactly one
// object is highlighted at a time (the counterpart of `applySelection` clearing
// wires). Trunk fan-outs (#117) add two rules: selecting a
// bundle also lights its member stubs (they carry the bundle in
// `data-trunk-path`), and selecting a member stub passes its bundle as `trunk`
// so the trunk lights with it — sibling stubs match neither rule and stay dark.
function selectWire(netPath: string, trunk?: string) {
  state.selected = null;
  applySelection();
  const host = $("schematic");
  host.querySelectorAll<SVGElement>(".wire, .wire-label").forEach((el) => {
    el.classList.toggle(
      "sel",
      el.dataset.netPath === netPath ||
        el.dataset.trunkPath === netPath ||
        (trunk !== undefined && el.dataset.netPath === trunk),
    );
  });
}

// -- apply a cross-probe result to source + waveform -----------------------

// The one bus subscriber (#18): every cross-pane selection — from this window or,
// once panes detach, another — lands here and drives the panes this window hosts.
// A `scope` selection drills the schematic (tree jump); a `resp` selection reveals
// the resolved cross-probe in whichever panes it targets. This is the single
// coordination path that the right-click/tree handlers publish into.
async function handleSelection(sel: Selection) {
  // Drive only the panes this window owns (source → main; schematic/waveform → the
  // window whose label matches the selection's `dest`, or main's own on a broadcast).
  // This is what keeps a selection from being applied in more than one window.
  const owns = (t: RevealTarget) =>
    sel.targets.includes(t) &&
    ownsSelection({ mode: windowMode, self: selfLabel }, t, sel.dest, []);
  if (sel.scope !== null && owns("schematic")) navToScope(sel.scope);
  if (sel.resp) {
    if (owns("source")) await showInSource(sel.resp);
    if (owns("schematic")) await showInSchematic(sel.resp.anchor);
    if (owns("waveform")) await appendResolved(sel.resp);
  }
}

// Append a cross-probed signal to *this* window's waveform, resolved against its own
// trace (#170). A waveform pop-out may hold a different trace than the origin window,
// so the origin's `signal_ref` isn't portable — re-resolve by the model node path
// (the single source of truth); a signal absent from this trace simply adds no lane.
async function appendResolved(resp: ProbeResponse) {
  if (sid === undefined) {
    // main / default session already resolved this response against its own trace.
    await addToWaveform(resp.wave, resp.anchor.path);
    return;
  }
  try {
    const local = await api.probeNode(resp.anchor.path, null, sid);
    if (local?.wave) await addToWaveform(local.wave, resp.anchor.path);
  } catch (e) {
    log("error", `probe failed: ${e}`);
  }
}

// Show a cross-probe result in the source pane (jump to its location, if any) and
// list any ambiguous alternatives. Waveform display is now an explicit, additive
// action (the menu's "Append to waveform"), so it is no longer touched here.
async function showInSource(resp: ProbeResponse) {
  activateTab("source-pane"); // reveal + focus the source tab (#99)
  // Surface a source read failure (missing .sv, wrong src-root) in the status log
  // instead of leaving the pane silently empty — the menu path calls this without
  // its own catch, so an unhandled `source_text` rejection otherwise vanishes.
  if (resp.source) {
    try {
      await renderSource(
        resp.source.file,
        resp.source.line,
        resp.source.offset,
        resp.source.end_offset,
      );
    } catch (e) {
      log("warn", `source unavailable: ${e}`);
    }
  }
  renderPicker(resp);
}

async function renderSource(
  file: number,
  line: number,
  startOffset?: number,
  endOffset?: number,
) {
  let lines = state.source.get(file);
  if (!lines) {
    const text = await api.sourceText(file);
    // Normalize CRLF/CR to LF so a line is one newline byte — keeps computed byte
    // offsets aligned with the model's source ranges (slang counts LF), regardless
    // of the on-disk line endings on this platform.
    lines = text.split(/\r\n|\r|\n/);
    state.source.set(file, lines);
  }
  // Byte offset at the start of each line (LF newline = 1 byte), for right-click
  // → byte-offset resolution via probe_source.
  const lineStarts = new Array<number>(lines.length);
  let off = 0;
  for (let i = 0; i < lines.length; i++) {
    lineStarts[i] = off;
    off += lines[i].length + 1;
  }
  sourceCtx = { file, lineStarts };

  // Highlight the construct's whole span (#158): the probe carries the full
  // byte range (offset..end_offset) of the def, so light every line it covers,
  // not just its header. Falls back to the single start line when no span is
  // given. lineRangeForSpan yields inclusive 0-based [startLine, endLine].
  const [hlStart, hlEnd] =
    startOffset !== undefined && endOffset !== undefined
      ? lineRangeForSpan(lineStarts, startOffset, endOffset)
      : [line - 1, line - 1];

  const host = $("source");
  host.innerHTML = "";
  lines.forEach((text, i) => {
    const div = document.createElement("div");
    div.className = "line" + (i >= hlStart && i <= hlEnd ? " hl" : "");
    div.dataset.lineIndex = String(i);
    div.innerHTML = `<span class="ln">${i + 1}</span>`;
    div.appendChild(document.createTextNode(text));
    host.appendChild(div);
  });
  host.querySelector(".hl")?.scrollIntoView({ block: "center" });
}

const RULER_H = 16;

// The visible time window, defaulting to the full data window when unset.
function currentView(): TimeWindow {
  return state.waveView ?? { t0: 0, t1: maxTime(laneList()) };
}

// Rebuild the waveform pane: a top ruler row, then, per group (#182), a collapsible
// header row followed by its lanes (unless collapsed) — each lane a grid row of
// name | value-at-A | track | reorder (↑/↓) + remove (×). Track/ruler cells are canvases
// drawn by `redrawTracks` on the shared visible window; the flat grid keeps the time
// columns aligned across every row (#15, #16). The pane is always ≥1 group with an empty
// trailing one, so there is no "(no signals)" state — a fresh pane shows an empty group.
function renderWaves() {
  const list = $("wave-list");
  normalizeWaveGroups(); // guarantee the always-present empty trailing group
  list.innerHTML = "";
  list.classList.add("has-rows");
  // Ruler row: spacers flank a track-column canvas so it aligns with the tracks.
  const rulerCell = document.createElement("div");
  rulerCell.className = "wave-ruler-cell";
  const ruler = document.createElement("canvas");
  ruler.className = "wave-ruler";
  rulerCell.appendChild(ruler);
  list.append(spacer(), spacer(), rulerCell, spacer());

  for (const group of state.groups) {
    renderGroupHeader(list, group);
    if (group.collapsed) continue;
    group.waves.forEach((tr, i) =>
      renderLaneRow(list, tr, i === 0, i === group.waves.length - 1),
    );
  }
  redrawTracks();
}

// A full-width group header row: collapse twist, name (double-click to rename), and a
// lane count. The empty trailing group reads as a drop target for a new group.
function renderGroupHeader(list: HTMLElement, group: WaveGroup) {
  const header = document.createElement("div");
  header.className = "wave-group-header";
  const twist = document.createElement("button");
  twist.className = "wave-twist";
  twist.textContent = group.collapsed ? "▸" : "▾";
  twist.title = group.collapsed ? "Expand group" : "Collapse group";
  twist.onclick = () => {
    group.collapsed = !group.collapsed;
    renderWaves();
  };
  const name = document.createElement("span");
  name.className = "wave-group-name";
  const empty = group.waves.length === 0;
  name.textContent = empty ? `${group.name} — drop signals here` : group.name;
  if (empty) name.classList.add("empty");
  name.title = "Double-click to rename";
  name.ondblclick = () => renameGroup(group, name);
  const count = document.createElement("span");
  count.className = "wave-group-count";
  if (!empty) count.textContent = `${group.waves.length}`;
  header.append(twist, name, count);
  list.append(header);
}

// One lane: name | value@A | track | ↑/↓/×. Controls address the lane by its stable key
// (#179) so they stay correct with duplicate refs and across groups.
function renderLaneRow(list: HTMLElement, tr: WaveTrace, first: boolean, last: boolean) {
  const name = document.createElement("div");
  name.className = "wave-row-name";
  name.textContent = tr.name;
  name.title = tr.name;
  // Right-click the name (not the track — that drops marker B) for per-signal value
  // formatting: change radix / add another view / move to group / create a sub-bus.
  name.oncontextmenu = (e) => {
    e.preventDefault();
    openSignalMenu(e, tr.key);
  };
  name.appendChild(colResizer("name"));

  const value = document.createElement("div");
  value.className = "wave-row-value";
  // The value text lives in a span so redrawTracks can update it without wiping the
  // resizer (textContent on the cell would remove all children).
  const valueLbl = document.createElement("span");
  valueLbl.className = "wave-val-lbl";
  value.append(valueLbl, colResizer("value"));

  const cell = document.createElement("div");
  cell.className = "wave-track-cell";
  const canvas = document.createElement("canvas");
  canvas.className = "wave-track";
  cell.appendChild(canvas);

  const ctrls = document.createElement("div");
  ctrls.className = "wave-ctrls";
  ctrls.appendChild(waveBtn("↑", first, () => moveLane(tr.key, -1)));
  ctrls.appendChild(waveBtn("↓", last, () => moveLane(tr.key, 1)));
  ctrls.appendChild(waveBtn("×", false, () => removeLane(tr.key)));

  list.append(name, value, cell, ctrls);
}

// Rename a group in place: swap the label for an input, commit on Enter/blur, cancel on
// Escape. `renderWaves` clears `#wave-list`, which removes the focused input and fires a
// synchronous blur — so `finish` is guarded (and detaches `onblur`) to run exactly once
// and not re-enter `renderWaves` mid-rebuild.
function renameGroup(group: WaveGroup, label: HTMLElement) {
  const input = document.createElement("input");
  input.className = "wave-group-rename";
  input.value = group.name;
  let done = false;
  const finish = (save: boolean) => {
    if (done) return;
    done = true;
    input.onblur = null;
    if (save) {
      const v = input.value.trim();
      if (v) group.name = v;
    }
    renderWaves();
  };
  input.onkeydown = (e) => {
    if (e.key === "Enter") finish(true);
    else if (e.key === "Escape") finish(false);
  };
  input.onblur = () => finish(true);
  label.replaceWith(input);
  input.focus();
  input.select();
}

function spacer(): HTMLDivElement {
  const d = document.createElement("div");
  d.className = "wave-spacer";
  return d;
}

// Minimum widths (px) for the resizable columns, to stop them collapsing.
const COL_MIN = { name: 60, value: 40 };

// A drag handle on a column's right edge; click-hold and drag to resize (#84).
function colResizer(col: "name" | "value"): HTMLDivElement {
  const r = document.createElement("div");
  r.className = "col-resizer";
  r.onmousedown = (ev) => startColResize(col, ev);
  return r;
}

function startColResize(col: "name" | "value", ev: MouseEvent) {
  ev.preventDefault();
  ev.stopPropagation();
  const cell = (ev.currentTarget as HTMLElement).parentElement!;
  const startX = ev.clientX;
  const startW = cell.getBoundingClientRect().width;
  const onMove = (e: MouseEvent) => {
    state.waveCol[col] = Math.max(COL_MIN[col], startW + (e.clientX - startX));
    applyColWidths();
  };
  const onUp = () => {
    window.removeEventListener("mousemove", onMove);
    window.removeEventListener("mouseup", onUp);
    persistColWidths();
  };
  window.addEventListener("mousemove", onMove);
  window.addEventListener("mouseup", onUp);
}

// Push the column widths into CSS custom properties the grid reads (unset → the CSS
// minmax default).
function applyColWidths() {
  const list = $("wave-list");
  for (const col of ["name", "value"] as const) {
    const w = state.waveCol[col];
    if (w) list.style.setProperty(`--wave-col-${col}`, `${w}px`);
    else list.style.removeProperty(`--wave-col-${col}`);
  }
}

function persistColWidths() {
  try {
    localStorage.setItem("waveCol", JSON.stringify(state.waveCol));
  } catch {
    /* ignore persistence failure */
  }
}

function loadColWidths() {
  try {
    const saved = localStorage.getItem("waveCol");
    if (saved) state.waveCol = JSON.parse(saved);
  } catch {
    /* ignore malformed/persisted-state failure */
  }
  applyColWidths();
}

// Row splitter between the content row (schematic/source) and the bottom waveform
// drawer (#98). Drag to resize: the height lives in the --bottom-h grid track on
// #panes and is persisted. Mirrors the #84 column-resizer pattern above.
const BOTTOM_MIN = 80; // px — keep the drawer usable
const TOP_MIN = 120; // px — keep the content row from collapsing

function setupRowSplitter() {
  $("row-splitter").addEventListener("mousedown", startRowResize);
}

function startRowResize(ev: MouseEvent) {
  ev.preventDefault();
  const panes = $("panes");
  const onMove = (e: MouseEvent) => {
    const rect = panes.getBoundingClientRect();
    const max = Math.max(BOTTOM_MIN, rect.height - TOP_MIN);
    const h = Math.min(Math.max(BOTTOM_MIN, rect.bottom - e.clientY), max);
    panes.style.setProperty("--bottom-h", `${h}px`);
  };
  const onUp = () => {
    window.removeEventListener("mousemove", onMove);
    window.removeEventListener("mouseup", onUp);
    persistRowSplit();
  };
  window.addEventListener("mousemove", onMove);
  window.addEventListener("mouseup", onUp);
}

function persistRowSplit() {
  try {
    const h = $("panes").style.getPropertyValue("--bottom-h");
    if (h) localStorage.setItem("bottomRowH", h);
  } catch {
    /* ignore persistence failure */
  }
}

function loadRowSplit() {
  try {
    const saved = localStorage.getItem("bottomRowH");
    if (saved) $("panes").style.setProperty("--bottom-h", saved);
  } catch {
    /* ignore malformed/persisted-state failure */
  }
}

// Column splitter between the tree column and the content column (#139). Drag to
// resize: the width lives in the --tree-w grid track on #panes and is persisted.
// Mirrors setupRowSplitter above; computes width from the pointer's x relative to
// #panes rather than the bottom edge. `startColSplit` (not `startColResize`, the #84
// waveform column resizer) keeps the two drag handlers distinct.
const TREE_MIN = 140; // px — keep the tree pane usable
const CONTENT_MIN = 240; // px — keep the content column from collapsing

function setupColSplitter() {
  $("col-splitter").addEventListener("mousedown", startColSplit);
}

function startColSplit(ev: MouseEvent) {
  ev.preventDefault();
  const panes = $("panes");
  const onMove = (e: MouseEvent) => {
    const rect = panes.getBoundingClientRect();
    const max = Math.max(TREE_MIN, rect.width - CONTENT_MIN);
    const w = Math.min(Math.max(TREE_MIN, e.clientX - rect.left), max);
    panes.style.setProperty("--tree-w", `${w}px`);
  };
  const onUp = () => {
    window.removeEventListener("mousemove", onMove);
    window.removeEventListener("mouseup", onUp);
    persistColSplit();
  };
  window.addEventListener("mousemove", onMove);
  window.addEventListener("mouseup", onUp);
}

function persistColSplit() {
  try {
    const w = $("panes").style.getPropertyValue("--tree-w");
    if (w) localStorage.setItem("treeColW", w);
  } catch {
    /* ignore persistence failure */
  }
}

function loadColSplit() {
  try {
    const saved = localStorage.getItem("treeColW");
    if (saved) $("panes").style.setProperty("--tree-w", saved);
  } catch {
    /* ignore malformed/persisted-state failure */
  }
}

// Rescale the waveform canvases whenever the wave list's box changes — a window resize
// or a row-splitter drag (#98), which fires no window "resize" event. A ResizeObserver
// catches both; rAF-coalesced so a burst of size changes triggers a single redraw.
// The schematic is deliberately NOT re-fitted on resize: enlarging the pane keeps the
// drawing at its current scale (signal spacing preserved) and simply reveals empty space
// below, instead of zooming the whole schematic up. Explicit fit stays on the zoom-reset
// button / Ctrl+0 (#114).
function setupResizeRedraw() {
  let pending = false;
  const ro = new ResizeObserver(() => {
    if (pending) return;
    pending = true;
    requestAnimationFrame(() => {
      pending = false;
      redrawTracks();
    });
  });
  ro.observe($("wave-list"));
}

// Size each canvas to its laid-out cell and draw on the current window. Reading
// clientWidth forces the layout needed to get the real column width; called after a
// rebuild, on resize, and on every zoom/pan/marker change (no DOM rebuild needed).
function redrawTracks() {
  const list = $("wave-list");
  const view = currentView();
  const scale = displayScale(state.timescale, state.waveUnit);
  const ruler = list.querySelector<HTMLCanvasElement>(".wave-ruler");
  if (ruler) {
    ruler.width = Math.max(1, ruler.clientWidth);
    ruler.height = RULER_H;
    drawRuler(ruler, view, state.markers, scale);
  }
  const canvases = list.querySelectorAll<HTMLCanvasElement>(".wave-track");
  const valueLbls = list.querySelectorAll<HTMLElement>(".wave-val-lbl");
  // Only non-collapsed groups render tracks, so map against the visible lanes (#182).
  visibleLaneList().forEach((tr, i) => {
    const radix = tr.radix ?? "hex";
    const canvas = canvases[i];
    if (canvas) {
      canvas.width = Math.max(1, canvas.clientWidth);
      canvas.height = TRACK_H;
      drawTrack(canvas, tr.values, view, state.markers, radix, tr.enumMap, tr.showName);
    }
    const vc = valueLbls[i];
    // Value at the primary marker A (prev -> next when A sits on a transition); the
    // latest value when A is unset. Formatted as the state name or the trace's radix.
    if (vc) {
      vc.textContent =
        state.markers.a == null
          ? displayValue(
              valueAt(tr.values, Number.POSITIVE_INFINITY),
              radix,
              tr.enumMap,
              tr.showName,
            )
          : valueAtMarker(tr.values, state.markers.a, radix, tr.enumMap, tr.showName);
    }
  });
  updateMarkerReadout();
}

// Header readout: A/B timestamps and their delta, in the selected display unit.
function updateMarkerReadout() {
  const { a, b } = state.markers;
  const scale = displayScale(state.timescale, state.waveUnit);
  const u = state.timescale ? ` ${state.waveUnit}` : "";
  const fmt = (t: number) => `${Math.round(t * scale * 100) / 100}${u}`;
  const parts: string[] = [];
  if (a != null) parts.push(`A ${fmt(a)}`);
  if (b != null) parts.push(`B ${fmt(b)}`);
  if (a != null && b != null) parts.push(`Δ ${fmt(Math.abs(b - a))}`);
  $("wave-readout").textContent = parts.join("   ");
}

// Mirror the selected display unit into the header dropdown.
function syncUnitSelect() {
  const sel = document.getElementById("wave-unit") as HTMLSelectElement | null;
  if (sel) sel.value = state.waveUnit;
}

function waveBtn(label: string, disabled: boolean, onClick: () => void): HTMLButtonElement {
  const b = document.createElement("button");
  b.className = "wave-btn";
  b.textContent = label;
  b.disabled = disabled;
  b.onclick = onClick;
  return b;
}

// Move a lane one slot within its own group (#182 keeps reorder inside a group; regroup
// is the name-cell menu / #188 drag). Addressed by stable key so duplicate refs are safe.
function moveLane(key: number, dir: number) {
  const found = findLane(key);
  if (!found) return;
  const { group, index } = found;
  const j = index + dir;
  if (j < 0 || j >= group.waves.length) return;
  [group.waves[index], group.waves[j]] = [group.waves[j], group.waves[index]];
  renderWaves();
}

function removeLane(key: number) {
  const found = findLane(key);
  if (!found) return;
  found.group.waves.splice(found.index, 1);
  renderWaves(); // normalizeWaveGroups (in renderWaves) prunes a now-empty interior group
}

const RADIX_LABELS: { r: Radix; label: string }[] = [
  { r: "hex", label: "Hex" },
  { r: "dec", label: "Decimal" },
  { r: "oct", label: "Octal" },
  { r: "bin", label: "Binary" },
];

// Derived sub-bus tracks get unique negative refs so they never collide with real
// u32 signal_refs. Lane keys (#179) count up from 1 — a lane's stable identity now that
// several lanes can share a `ref`. Group names (#182) count up too. All are per-window
// counters; `reseedLaneCounters` resumes them past a snapshot's restored lanes/groups so
// a pop-out never re-mints a collision.
let derivedSeq = -1;
let laneSeq = 1;
let groupSeq = 1;
function nextLaneKey(): number {
  return laneSeq++;
}
function nextGroupName(): string {
  return `Group ${groupSeq++}`;
}
function reseedLaneCounters(): void {
  const { laneKey, derivedRef } = laneCounterSeeds(flattenLanes(state.groups));
  laneSeq = laneKey;
  derivedSeq = derivedRef;
  // Resume group numbering past the largest `Group N` already present.
  for (const g of state.groups) {
    const m = /^Group (\d+)$/.exec(g.name);
    if (m && Number(m[1]) >= groupSeq) groupSeq = Number(m[1]) + 1;
  }
}

// --- #182 group helpers -------------------------------------------------------------

// Every pinned lane, flat and in order; and just the drawn ones (non-collapsed groups).
const laneList = (): WaveTrace[] => flattenLanes(state.groups);
const visibleLaneList = (): WaveTrace[] => visibleLanes(state.groups);

// Re-establish the pane invariant after any mutation: one trailing empty group, no
// interior empties, ≥1 group. Call before re-rendering.
function normalizeWaveGroups(): void {
  state.groups = normalizeGroups(state.groups, nextGroupName);
}

// Locate a lane by its stable key (#179) across all groups.
function findLane(key: number): { group: WaveGroup; index: number } | null {
  for (const group of state.groups) {
    const index = group.waves.findIndex((w) => w.key === key);
    if (index >= 0) return { group, index };
  }
  return null;
}

// The group a newly added signal lands in — the working group (last populated, or the
// default group of a fresh pane), guaranteed to exist by the invariant.
function workingGroup(): WaveGroup {
  if (state.groups.length === 0) normalizeWaveGroups();
  return state.groups[workingGroupIndex(state.groups)];
}

// Bit width of a trace when it is a sliceable bit-vector (binary value string), else 0.
function busWidth(tr: WaveTrace): number {
  const v = tr.values.find((c) => c.value.length > 0);
  return v && /^[01xz]+$/i.test(v.value) ? v.value.length : 0;
}

// Move a lane into an existing group (#182): splice from its current group, push onto
// the target. Re-found by key so a stale menu closure can't move the wrong lane.
function moveLaneToGroup(key: number, target: WaveGroup) {
  const found = findLane(key);
  if (!found || found.group === target) return;
  const [lane] = found.group.waves.splice(found.index, 1);
  target.waves.push(lane);
  renderWaves();
}

// Per-signal value-format menu (radix / another view / move to group / sub-bus), opened
// from the name cell (#78). Addressed by the lane's stable key (#179).
function openSignalMenu(ev: MouseEvent, key: number) {
  const found = findLane(key);
  if (!found) return;
  const tr = found.group.waves[found.index];
  const cur = tr.radix ?? "hex";
  const radixSubmenu: MenuItem[] = [];
  // Enum signals get a "State name" mode at the top of the submenu (default on).
  if (tr.enumMap) {
    radixSubmenu.push({
      label: `${tr.showName ? "✓ " : ""}State name`,
      enabled: true,
      onClick: () => {
        tr.showName = true;
        redrawTracks();
      },
    });
  }
  for (const { r, label } of RADIX_LABELS) {
    radixSubmenu.push({
      label: `${!tr.showName && r === cur ? "✓ " : ""}${label}`,
      enabled: true,
      onClick: () => {
        tr.radix = r;
        tr.showName = false; // picking a numeric radix leaves name mode
        redrawTracks();
      },
    });
  }
  // Move to any other group — including the empty trailing group, which is how you start
  // a new group: dropping a lane into it populates it and the invariant spawns a fresh
  // empty group below (the non-drag path; #188 adds the drag gesture). Only the lane's
  // own group is omitted.
  const groupSubmenu: MenuItem[] = state.groups
    .filter((g) => g !== found.group)
    .map((g) => ({
      label: g.waves.length === 0 ? `${g.name} (new group)` : g.name,
      enabled: true,
      onClick: () => moveLaneToGroup(key, g),
    }));
  const width = busWidth(tr);
  openContextMenu(ev.clientX, ev.clientY, [
    { label: "Change radix", enabled: true, submenu: radixSubmenu },
    {
      // Add the same signal as a second lane so it can be read a different way at once
      // (#179) — a bus as hex *and* state name, hex next to binary. Bypasses the append
      // dedupe (which stays the default for picker/schematic clicks). Starts on numeric
      // hex so the clone reads differently from the original by default.
      label: "Add another view",
      enabled: true,
      onClick: () => {
        const f = findLane(key);
        if (!f) return;
        f.group.waves.splice(f.index + 1, 0, {
          ...tr,
          key: nextLaneKey(),
          radix: "hex",
          showName: false,
        });
        renderWaves();
      },
    },
    { label: "Move to group", enabled: true, submenu: groupSubmenu },
    {
      label: width > 1 ? "Create sub-bus…" : "Create sub-bus… (not a bus)",
      enabled: width > 1,
      onClick: () => openSubBusPopover(ev, key, width),
    },
  ]);
}

// Insert a derived track of parent[hi:lo] right after the parent, in the parent's group.
// When the parent is a plain signal, the sub-bus records the parent path + slice so a
// trace swap re-derives it (see reresolveLane); a sub-bus of a sub-bus can't be
// re-derived from a single path, so it carries neither and is dropped on a swap (#179).
function makeSubBus(key: number, hi: number, lo: number) {
  const found = findLane(key);
  if (!found) return;
  const tr = found.group.waves[found.index];
  const values = tr.values.map((c) => ({ time: c.time, value: sliceBits(c.value, hi, lo) }));
  const derivable = tr.path !== undefined && tr.slice === undefined;
  found.group.waves.splice(found.index + 1, 0, {
    key: nextLaneKey(),
    ref: derivedSeq--,
    name: `${tr.name}[${hi}:${lo}]`,
    path: derivable ? tr.path : undefined,
    slice: derivable ? { hi, lo } : undefined,
    values,
    radix: tr.radix ?? "hex",
  });
  renderWaves();
}

// Small inline popover to pick the [hi:lo] bit range for a sub-bus.
function openSubBusPopover(ev: MouseEvent, key: number, width: number) {
  closeContextMenu();
  document.getElementById("subbus-pop")?.remove();
  const pop = document.createElement("div");
  pop.id = "subbus-pop";
  pop.innerHTML =
    `<label>bits [<input type="number" class="hi" min="0" max="${width - 1}" value="${width - 1}">` +
    `:<input type="number" class="lo" min="0" max="${width - 1}" value="0">]</label>`;
  const ok = document.createElement("button");
  ok.textContent = "Add";
  const cancel = document.createElement("button");
  cancel.textContent = "Cancel";
  pop.append(ok, cancel);
  pop.style.left = `${ev.clientX}px`;
  pop.style.top = `${ev.clientY}px`;
  document.body.appendChild(pop);
  const hiIn = pop.querySelector<HTMLInputElement>(".hi")!;
  const loIn = pop.querySelector<HTMLInputElement>(".lo")!;
  hiIn.focus();
  const close = () => pop.remove();
  cancel.onclick = close;
  ok.onclick = () => {
    let hi = Number(hiIn.value);
    let lo = Number(loIn.value);
    if (!Number.isFinite(hi) || !Number.isFinite(lo)) return close();
    if (hi < lo) [hi, lo] = [lo, hi];
    hi = Math.min(Math.max(hi, 0), width - 1);
    lo = Math.min(Math.max(lo, 0), width - 1);
    close();
    makeSubBus(key, hi, lo);
  };
  pop.addEventListener("keydown", (e) => {
    if (e.key === "Enter") ok.click();
    else if (e.key === "Escape") close();
  });
}

function renderPicker(resp: ProbeResponse) {
  const pick = $("picker");
  if (!resp.alternatives.length) {
    pick.style.display = "none";
    return;
  }
  pick.innerHTML = `<b>also matches (${resp.alternatives.length}):</b>`;
  for (const alt of resp.alternatives) {
    const d = document.createElement("div");
    d.className = "alt";
    d.textContent = alt.path;
    d.onclick = () =>
      api
        .probeNode(alt.path, context())
        .then((r) => r && publish(crossProbeSelection(r, ["source"])));
    pick.appendChild(d);
  }
  pick.style.display = "block";
}

// -- source right-click → schematic / waveform (#19) -----------------------

// Resolve a screen point in the source pane to a file byte offset, using the
// caret position under the cursor plus the line's precomputed start offset.
function sourceOffsetAt(x: number, y: number): number | null {
  if (!sourceCtx) return null;
  const doc = document as Document & {
    caretRangeFromPoint?: (x: number, y: number) => Range | null;
    caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
  };
  let node: Node | null = null;
  let col = 0;
  if (doc.caretRangeFromPoint) {
    const r = doc.caretRangeFromPoint(x, y);
    if (r) [node, col] = [r.startContainer, r.startOffset];
  } else if (doc.caretPositionFromPoint) {
    const p = doc.caretPositionFromPoint(x, y);
    if (p) [node, col] = [p.offsetNode, p.offset];
  }
  if (!node) return null;
  const el = node.nodeType === Node.TEXT_NODE ? node.parentElement : (node as Element);
  const lineDiv = el?.closest<HTMLElement>(".line");
  if (!lineDiv?.dataset.lineIndex) return null;
  const start = sourceCtx.lineStarts[Number(lineDiv.dataset.lineIndex)];
  // A click on the line-number gutter resolves to the start of the line's code.
  return el?.closest(".ln") ? start : start + col;
}

function closeContextMenu() {
  $("ctxmenu").style.display = "none";
  document.getElementById("ctxsubmenu")?.remove();
}

interface MenuItem {
  label: string;
  enabled: boolean;
  onClick?: () => void;
  submenu?: MenuItem[]; // when set, the item opens a nested menu on hover
}

// Render items into a menu panel. Items with a submenu show a "▸" and open a flyout
// on hover; leaf items run their onClick and close everything. On the root panel,
// hovering a leaf dismisses any open submenu (submenu panels skip that so hovering
// their own items doesn't close them).
function renderMenuItems(menu: HTMLElement, items: MenuItem[], isRoot: boolean) {
  menu.innerHTML = "";
  for (const item of items) {
    const hasSub = !!item.submenu?.length;
    const d = document.createElement("div");
    d.className = "ctx-item" + (item.enabled ? "" : " disabled");
    const label = document.createElement("span");
    label.textContent = item.label;
    d.appendChild(label);
    if (hasSub) {
      const arrow = document.createElement("span");
      arrow.className = "ctx-arrow";
      arrow.textContent = "▸";
      d.appendChild(arrow);
      d.onmouseenter = () => openSubmenu(d, item.submenu!);
    } else if (item.enabled) {
      if (isRoot) d.onmouseenter = () => document.getElementById("ctxsubmenu")?.remove();
      d.onclick = () => {
        closeContextMenu();
        item.onClick?.();
      };
    }
    menu.appendChild(d);
  }
}

// Position a menu panel at (x, y), clamped into the viewport.
function placeMenu(menu: HTMLElement, x: number, y: number) {
  menu.style.left = `${x}px`;
  menu.style.top = `${y}px`;
  menu.style.display = "block";
  const r = menu.getBoundingClientRect();
  if (r.right > window.innerWidth)
    menu.style.left = `${Math.max(0, window.innerWidth - r.width)}px`;
  if (r.bottom > window.innerHeight)
    menu.style.top = `${Math.max(0, window.innerHeight - r.height)}px`;
}

// Open a one-level flyout to the right of `anchor` (flips left if it would overflow).
function openSubmenu(anchor: HTMLElement, items: MenuItem[]) {
  document.getElementById("ctxsubmenu")?.remove();
  const sub = document.createElement("div");
  sub.id = "ctxsubmenu";
  renderMenuItems(sub, items, false);
  document.body.appendChild(sub);
  const r = anchor.getBoundingClientRect();
  placeMenu(sub, r.right, r.top);
  const sr = sub.getBoundingClientRect();
  if (sr.right > window.innerWidth) sub.style.left = `${Math.max(0, r.left - sr.width)}px`;
}

function openContextMenu(x: number, y: number, items: MenuItem[]) {
  document.getElementById("ctxsubmenu")?.remove();
  const menu = $("ctxmenu");
  renderMenuItems(menu, items, true);
  placeMenu(menu, x, y);
}

// Left-click a source line to move the highlight to just that line (#163). A
// lightweight cursor affordance: shift the `.hl` marker to the clicked line — no
// probe, no re-render, no scroll jump, so it doesn't re-anchor to (or highlight) the
// whole construct block the way "Show in source" (#158) does. Right-click still owns
// the model cross-probe menu (Show in schematic / Append to waveform).
function onSourceClick(ev: MouseEvent) {
  // Ignore a click that ends a text drag-selection (user is selecting to copy),
  // so re-highlighting doesn't fight the selection.
  if (!window.getSelection()?.isCollapsed) return;
  const host = $("source");
  const line = (ev.target as Element | null)?.closest(".line");
  if (!line || !host.contains(line)) return;
  host.querySelectorAll(".line.hl").forEach((el) => el.classList.remove("hl"));
  line.classList.add("hl");
}

// Right-click in the source pane: resolve the signal/object under the cursor and
// offer "Show in schematic" / "Append to waveform" (the latter disabled when the
// object has no trace signal).
async function onSourceContextMenu(ev: MouseEvent) {
  ev.preventDefault();
  const offset = sourceOffsetAt(ev.clientX, ev.clientY);
  if (offset == null || !sourceCtx) return;
  const resp = await api.probeSource(sourceCtx.file, offset, context());
  if (!resp) return; // nothing resolvable at this position
  openContextMenu(ev.clientX, ev.clientY, [
    {
      label: "Show in schematic",
      enabled: true,
      onClick: () => void publish(crossProbeSelection(resp, ["schematic"])),
    },
    await appendWaveItem(resp),
  ]);
}

// Navigate the schematic to show `anchor`: drill into it if it is itself a scope,
// else open the nearest enclosing scope and highlight the box/wire it maps to.
async function showInSchematic(anchor: NodeRef) {
  // Reveal + focus the schematic tab first, so it renders into a sized pane (a
  // hidden 0-width host would fit wrong) and scrollIntoView lands correctly (#99).
  activateTab("schematic-pane");
  const segs = anchor.path.split(".");
  for (let n = segs.length; n >= 1; n--) {
    const scopePath = segs.slice(0, n).join(".");
    let graph: SchematicGraph | null = null;
    try {
      graph = await api.scopeGraph(scopePath);
    } catch {
      continue; // not a navigable scope — walk up
    }
    rememberCurrentView();
    state.stack = scopeFrames(scopePath);
    state.graph = graph;
    state.selected = null;
    renderBreadcrumb();
    // Keep the current zoom level (don't zoom-to-fit), so the item is shown at the
    // zoom the user is already working at; scroll it into view below.
    await renderSchematic(graph, { k: zoom.k, scrollLeft: 0, scrollTop: 0 });
    // Highlight the anchor within the opened scope (a box by id, a net by path)
    // and centre it; when we drilled into the anchor itself there is nothing to
    // highlight. Selection is single-object, so pick the channel that matches:
    // a drawn node wins, otherwise try the anchor's path as a net.
    if (scopePath !== anchor.path) {
      const host = $("schematic");
      const boxEl = host.querySelector<SVGGraphicsElement>(
        `[data-node-id="${anchor.id}"]`,
      );
      if (boxEl) selectNode(anchor.id);
      else selectWire(anchor.path);
      const el = boxEl ?? host.querySelector<SVGGraphicsElement>(".wire.sel");
      el?.scrollIntoView({ block: "center", inline: "center" });
    }
    return;
  }
}

// Append the signal as a new waveform lane; a no-op when the object has no trace
// signal. Lanes stack in append order (#15). Deduped by trace ref by default — a
// picker/schematic re-click of an already-pinned signal is a no-op — while the name
// cell's "Add another view" (#179) deliberately bypasses this to stack a second lane.
async function addToWaveform(wave: WaveLink, path?: string) {
  if (!wave.in_trace) return;
  if (laneList().some((w) => w.ref === wave.signal_ref)) return;
  const values = await api.signalValues(wave.signal_ref, sid);
  // Enum-typed signals carry a value→name map; show the state name by default.
  const enumMap = wave.enum_map
    ? new Map(wave.enum_map.map((m) => [m.value, m.name]))
    : undefined;
  // New signals land in the working group (last populated, or the default group of a
  // fresh pane) — they accumulate there; the trailing empty group is for new groups (#182).
  workingGroup().waves.push({
    key: nextLaneKey(),
    ref: wave.signal_ref,
    name: wave.full_name,
    path,
    values,
    radix: "hex",
    enumMap,
    showName: enumMap !== undefined,
  });
  renderWaves();
  activateTab("wave-pane"); // reveal + focus the waveform tab on append (#99)
}

// -- bootstrap -------------------------------------------------------------

// Dark is the default; Settings flips to a light schematic theme and persists it
// under the existing "theme" key (#17 folds the old toolbar toggle into the pane).
function applyStoredTheme() {
  try {
    if (localStorage.getItem("theme") === "light") {
      document.documentElement.dataset.theme = "light";
    }
  } catch {
    /* localStorage may be unavailable; default dark is fine */
  }
}

function setTheme(theme: "light" | "dark") {
  const root = document.documentElement;
  if (theme === "light") root.dataset.theme = "light";
  else delete root.dataset.theme;
  try {
    localStorage.setItem("theme", theme);
  } catch {
    /* ignore persistence failure */
  }
}

// Populate the Settings pane (#17) from persisted prefs and wire each control back
// to prefs.ts. Theme applies live; excluded scopes take effect on the next load
// (read there via loadExcluded()).
function initSettings() {
  applyStoredTheme();

  const theme = $("set-theme") as HTMLSelectElement;
  theme.value =
    document.documentElement.dataset.theme === "light" ? "light" : "dark";
  theme.addEventListener("change", () =>
    setTheme(theme.value === "light" ? "light" : "dark"),
  );

  const excluded = $("set-excluded") as HTMLInputElement;
  excluded.value = formatExcluded(loadExcluded());
  excluded.addEventListener("change", () =>
    saveExcluded(parseExcluded(excluded.value)),
  );
}

// Waveform zoom / pan / marker interaction. Listeners are delegated on #wave-list so
// they survive the per-change canvas rebuilds. Pixel→time uses each canvas's laid-out
// rect (drawing width == clientWidth, so 1 CSS px == 1 device px — see redrawTracks).
function setupWaveInteraction() {
  const list = $("wave-list");
  const tMax = () => maxTime(laneList());

  const zoomBy = (factor: number, pivotT: number) => {
    state.waveView = zoomWindow(currentView(), factor, pivotT, tMax());
    redrawTracks();
  };
  $("wave-zoom-in").addEventListener("click", () => {
    const v = currentView();
    zoomBy(0.8, (v.t0 + v.t1) / 2);
  });
  $("wave-zoom-out").addEventListener("click", () => {
    const v = currentView();
    zoomBy(1.25, (v.t0 + v.t1) / 2);
  });
  $("wave-zoom-reset").addEventListener("click", () => {
    state.waveView = null;
    redrawTracks();
  });
  $("wave-unit").addEventListener("change", (ev) => {
    state.waveUnit = (ev.target as HTMLSelectElement).value as DisplayUnit;
    redrawTracks();
  });

  // Map a pointer event over a track/ruler canvas to a time, else null.
  const timeAt = (ev: MouseEvent): number | null => {
    const canvas = (ev.target as HTMLElement)?.closest<HTMLCanvasElement>(
      ".wave-track, .wave-ruler",
    );
    if (!canvas) return null;
    const r = canvas.getBoundingClientRect();
    const v = currentView();
    return xToTime(ev.clientX - r.left, v.t0, v.t1, r.width);
  };

  // Marker time: like `timeAt`, but snapped to the nearest signal edge — the clicked
  // track's edges, or (on the ruler) the nearest edge across all signals.
  const markerTimeAt = (ev: MouseEvent): number | null => {
    const canvas = (ev.target as HTMLElement)?.closest<HTMLCanvasElement>(
      ".wave-track, .wave-ruler",
    );
    if (!canvas) return null;
    const r = canvas.getBoundingClientRect();
    const v = currentView();
    const raw = xToTime(ev.clientX - r.left, v.t0, v.t1, r.width);
    // DOM tracks exist only for visible lanes (collapsed groups draw none), so map the
    // clicked canvas against the visible lane list (#182).
    const visible = visibleLaneList();
    let candidates = visible;
    if (canvas.classList.contains("wave-track")) {
      const idx = Array.from(list.querySelectorAll(".wave-track")).indexOf(canvas);
      if (idx >= 0 && visible[idx]) candidates = [visible[idx]];
    }
    let snapped = raw;
    let bestD = Infinity;
    for (const tr of candidates) {
      const e = nearestEdge(tr.values, raw);
      if (e != null && Math.abs(e - raw) < bestD) {
        bestD = Math.abs(e - raw);
        snapped = e;
      }
    }
    return snapped;
  };

  // Ctrl/⌘ + wheel zooms about the cursor; plain wheel scrolls the signal list.
  list.addEventListener(
    "wheel",
    (ev) => {
      if (!(ev.ctrlKey || ev.metaKey)) return;
      const t = timeAt(ev);
      if (t == null) return;
      ev.preventDefault();
      zoomBy(ev.deltaY > 0 ? 1.25 : 0.8, t);
    },
    { passive: false },
  );

  // Drag to pan; a click without drag drops the primary marker A.
  let drag: { startX: number; moved: boolean; view: TimeWindow; w: number } | null = null;
  list.addEventListener("mousedown", (ev) => {
    if (ev.button !== 0) return;
    const canvas = (ev.target as HTMLElement)?.closest<HTMLCanvasElement>(
      ".wave-track, .wave-ruler",
    );
    if (!canvas) return;
    drag = { startX: ev.clientX, moved: false, view: currentView(), w: canvas.getBoundingClientRect().width };
  });
  window.addEventListener("mousemove", (ev) => {
    if (!drag) return;
    const dx = ev.clientX - drag.startX;
    if (!drag.moved && Math.abs(dx) < 4) return; // threshold: distinguish click vs pan
    drag.moved = true;
    const span = drag.view.t1 - drag.view.t0;
    state.waveView = panWindow(drag.view, -(dx / drag.w) * span, tMax());
    redrawTracks();
  });
  window.addEventListener("mouseup", (ev) => {
    if (!drag) return;
    const wasDrag = drag.moved;
    drag = null;
    if (wasDrag) return;
    const t = markerTimeAt(ev);
    if (t == null) return;
    state.markers.a = t; // left-click → primary marker A (snapped to nearest edge)
    redrawTracks();
  });

  // Right-click over a track/ruler drops the secondary marker B (no browser menu).
  list.addEventListener("contextmenu", (ev) => {
    const t = markerTimeAt(ev);
    if (t == null) return;
    ev.preventDefault();
    state.markers.b = t;
    redrawTracks();
  });
}

// Boot a detached pane window (#18 PR2): reuse this same page in `?pane=` mode,
// showing only the one pane full-window (body.detached* CSS), seeded from the
// localStorage snapshot the main window wrote, and driven live by the bus. A
// schematic pop-out shares the main session (same design, #169); a waveform pop-out
// owns its own session on its own trace (#170).
async function initDetached(pane: DetachablePane) {
  applyStoredTheme();
  document.body.classList.add("detached", `detached-${pane}`);
  await subscribe(handleSelection);
  if (pane === "schematic") {
    setupZoom();
    activateTab("schematic-pane");
    // Seed this independent pane on the scope main was viewing when it popped out
    // (#169); thereafter it navigates only via its own local drill.
    const scope = localStorage.getItem(detachScopeKey(selfLabel));
    if (scope) navToScope(scope);
    return;
  }
  setupWaveInteraction();
  setupResizeRedraw();
  activateTab("wave-pane");
  const snap = readWaveSnapshot();
  // Boot this pane's own backend session on the same design main loaded (#170), so
  // its trace queries (signal_values / trace_timescale) resolve independently.
  if (snap?.load) {
    try {
      // The returned design top is what this pane's signal picker seeds its tree from
      // (#171) — a pop-out has no #hierarchy and no nav stack to read it off.
      state.top = await loadPaneSession(snap.load, selfLabel);
    } catch (e) {
      log("error", `pane load failed: ${e}`);
    }
  }
  try {
    state.timescale = await api.traceTimescale(sid);
  } catch {
    state.timescale = null;
  }
  state.waveUnit = defaultDisplayUnit(state.timescale);
  if (snap) {
    // Seeded lanes are valid as-is: the pane booted on main's current trace, so their
    // signal_refs still resolve. A later "Load trace…" re-resolves them by model path.
    // Groups round-trip (#182); a pre-#182 snapshot's flat `waves` migrates to one group.
    state.groups = loadGroups(snap);
    reseedLaneCounters(); // resume key/sub-bus-ref/group counters past the restored lanes
    state.waveView = snap.waveView ?? null;
    state.markers = snap.markers ?? { a: null, b: null };
    if (snap.waveUnit) state.waveUnit = snap.waveUnit;
  }
  // This pane's own trace picker (#170) — swaps only this pane's trace.
  $("load-trace").addEventListener("click", () => void loadTraceOnly());
  syncUnitSelect();
  loadColWidths();
  renderWaves();
  // This pane's own signal picker (#171), on this pane's session.
  setupPicker();
  await initPicker();
}

// Read this waveform pane's seed snapshot (its design load spec + lanes), written by
// the main window before it created the pop-out (#170).
function readWaveSnapshot(): WaveSnapshot | null {
  const raw = localStorage.getItem(detachWaveKey(selfLabel));
  if (!raw) return null;
  try {
    return JSON.parse(raw) as WaveSnapshot;
  } catch {
    return null; // malformed snapshot; start with an empty pane
  }
}

// (Re)load `id`'s backend session on a design (#170) — a pre-elaborated model, or a
// designlist re-elaborated by the harness. `id` undefined targets the "main" session.
// Returns the design top.
function loadPaneSession(spec: LoadSpec, id?: string): Promise<string> {
  return spec.mode === "filelist"
    ? api.elaborateAndLoad(
        spec.filelist,
        spec.top,
        spec.incdirs,
        spec.trace,
        spec.excluded,
        spec.srcRoot,
        id,
      )
    : api.loadDesign(spec.model, spec.trace, spec.excluded, spec.srcRoot, id);
}

// "Load trace…" in a waveform pane (#170): pick a VCD/FST/GHW and reload *this window's*
// session on it, keeping the design as-is, then re-resolve every existing lane against
// the new trace by model path (dropping any signal the new trace lacks). In the main
// window this swaps main's own trace without touching the toolbar's design load; in a
// waveform pop-out it swaps only that pane's trace, leaving main and every other pane
// untouched. Which session it hits is just `sid`.
async function loadTraceOnly() {
  // main reloads the design it last loaded; a pop-out reloads the one it was seeded with.
  const spec = sid === undefined ? (state.loaded ?? undefined) : readWaveSnapshot()?.load;
  if (!spec) {
    log("error", "load a design before loading a trace");
    return;
  }
  let picked: string | null;
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    picked = (await open({
      multiple: false,
      filters: [{ name: "trace", extensions: ["vcd", "fst", "ghw"] }],
    })) as string | null;
  } catch (e) {
    log("error", `file dialog failed: ${e}`);
    return;
  }
  if (!picked) return; // cancelled
  const next: LoadSpec = { ...spec, trace: picked };
  log("info", `loading trace ${picked}…`);
  try {
    // Swap only the trace (#176): the design is unchanged, so this skips the model
    // re-ingest — and, for a designlist, the whole re-elaboration — a full load pays.
    await api.loadTrace(picked, sid);
  } catch (e) {
    log("error", `trace load failed: ${e}`);
    return;
  }
  try {
    state.timescale = await api.traceTimescale(sid);
  } catch {
    state.timescale = null;
  }
  state.waveUnit = defaultDisplayUnit(state.timescale);
  // Re-resolve lanes against the new trace by model path, in place per group so grouping
  // survives the swap (#182); drop any lane the new trace doesn't carry. reresolveLane
  // keeps a sub-bus sliced (and its synthetic ref) rather than reverting it (#179).
  for (const group of state.groups) {
    const resolved: WaveTrace[] = [];
    for (const t of group.waves) {
      if (!t.path) continue; // no model path → can't re-resolve across traces
      try {
        const local = await api.probeNode(t.path, null, sid);
        if (local?.wave?.in_trace) {
          const values = await api.signalValues(local.wave.signal_ref, sid);
          resolved.push(reresolveLane(t, local.wave.signal_ref, values));
        }
      } catch {
        /* skip a lane that fails to re-resolve */
      }
    }
    group.waves = resolved;
  }
  normalizeWaveGroups(); // prune groups the swap emptied; keep the trailing empty
  state.waveView = null;
  state.markers = { a: null, b: null };
  if (sid === undefined) {
    // main: remember the swap so a later pop-out seeds on the trace now on screen, and
    // keep the toolbar's trace field honest about what is actually loaded.
    state.loaded = next;
    const field = document.getElementById("trace") as HTMLInputElement | null;
    if (field) field.value = picked;
  } else {
    // A pop-out persists the new trace *and* the re-resolved groups together, so
    // reopening it reseeds against the trace it is actually showing (pre-switch refs
    // would mis-render).
    const saved: WaveSnapshot = {
      load: next,
      groups: state.groups.map(storeGroup),
      waveView: state.waveView,
      markers: state.markers,
      waveUnit: state.waveUnit,
    };
    localStorage.setItem(detachWaveKey(selfLabel), JSON.stringify(saved));
  }
  syncUnitSelect();
  renderWaves();
  // The design is unchanged, so the picker's tree stands — rebuilding it would collapse
  // the user's expansion state for nothing. Only which signals the trace carries moved,
  // so refetch the listed scope's in_trace flags (and the "added" marks, since the swap
  // drops lanes the new trace lacks).
  if (pickerScope) await showScopeSignals(pickerScope);
  log("info", `loaded trace ${picked}`);
}

async function init() {
  // A detached pane window reuses this page in a single-pane mode (#18 PR2).
  if (windowMode !== "main") {
    await initDetached(windowMode);
    return;
  }
  ($("model") as HTMLInputElement).value =
    "../../fixtures/picorv32_soc/golden/hierarchy.json";
  ($("filelist") as HTMLInputElement).value = "../../fixtures/picorv32_soc/picorv32_soc.f";
  ($("top") as HTMLInputElement).value = "picorv32_soc";
  ($("trace") as HTMLInputElement).value =
    "../../fixtures/picorv32_soc/traces/picorv32_soc.fst";
  ($("srcroot") as HTMLInputElement).value = "../..";
  $("load").addEventListener("click", load);
  $("load-mode").addEventListener("change", syncLoadMode);
  syncLoadMode();
  // The single cross-pane coordination path (#18): right-click/tree handlers
  // publish a Selection, this subscriber drives the panes. Registered before the
  // startup auto-load so no early selection is missed.
  await subscribe(handleSelection);
  initSettings();
  setupZoom();
  setupWaveInteraction();
  syncUnitSelect();
  loadColWidths();
  loadRowSplit();
  setupRowSplitter();
  loadColSplit(); // #139
  setupColSplitter(); // #139
  setupPicker(); // #171 — main's waveform pane gets a signal picker like every pop-out
  // Tab groups (#99): a tab click activates its panel; the toolbar buttons reveal +
  // focus the on-demand schematic / waveform views.
  document.querySelectorAll<HTMLButtonElement>(".tab").forEach((b) =>
    b.addEventListener("click", () => activateTab(b.dataset.panel!)),
  );
  // Reveal main's own schematic tab (#169: schematic pop-outs are independent, so
  // main always keeps its own schematic — the button never chases a pop-out).
  $("show-schematic").addEventListener("click", () => activateTab("schematic-pane"));
  // Reveal main's own waveform tab (#170: waveform pop-outs are independent too, so
  // main always keeps its own waveform — the button never chases a pop-out).
  $("show-waveform").addEventListener("click", () => activateTab("wave-pane"));
  // Pop-out buttons in the schematic / waveform tab-aux (#18 PR2).
  $("pop-schematic").addEventListener("click", () => void popOut("schematic"));
  $("pop-waveform").addEventListener("click", () => void popOut("waveform"));
  // Swap main's own trace without re-entering the toolbar's design load (#170); the
  // same button in a pop-out swaps only that pane's trace.
  $("load-trace").addEventListener("click", () => void loadTraceOnly());
  renderWaves(); // show the empty-state "(no signals)" list until a trace is added
  // Source left-click re-highlight (#163) + right-click menu (#19), and dismissals.
  $("source").addEventListener("click", onSourceClick);
  $("source").addEventListener("contextmenu", onSourceContextMenu);
  document.addEventListener("click", closeContextMenu);
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeContextMenu();
  });
  // Track canvases are sized from their laid-out cell width and the schematic re-fits
  // to its pane (while still in fitted view, #114); a ResizeObserver rescales both on a
  // window resize OR a row-splitter drag (#98), which fires no window "resize" event.
  setupResizeRedraw();

  // Launched from the command line with a designlist (#136): the Tauri shell
  // parsed argv before the window opened. Prefill the filelist form from it
  // (incdirs re-joined with ";", the inverse of the split in load()) and
  // auto-load — the exact path a manual Load click takes. Guarded so a
  // browser-only dev server (no Tauri `invoke`) still falls through to the form.
  let startup: StartupArgs | null = null;
  try {
    startup = await api.startupArgs();
  } catch (e) {
    // Expected under a browser-only dev server (no Tauri `invoke`); log so a
    // real IPC/DTO regression on a CLI launch is still visible in devtools.
    console.warn("startup_args unavailable, using the load form:", e);
    startup = null;
  }
  if (startup) {
    ($("load-mode") as HTMLSelectElement).value = "filelist";
    ($("filelist") as HTMLInputElement).value = startup.filelist;
    ($("top") as HTMLInputElement).value = startup.top;
    ($("incdir") as HTMLInputElement).value = startup.incdirs.join(";");
    ($("trace") as HTMLInputElement).value = startup.trace;
    ($("srcroot") as HTMLInputElement).value = startup.src_root;
    syncLoadMode();
    await load();
  }
}

document.addEventListener("DOMContentLoaded", () => void init());

// Keep nodeId referenced for potential external callers / tree-shaking clarity.
export { nodeId };
