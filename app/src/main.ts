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
  StartupArgs,
  TraceTimescale,
  TreeNode,
  WaveLink,
} from "./types";
import { scopeFrames } from "./tree";
import {
  defaultDisplayUnit,
  displayScale,
  displayValue,
  drawTrack,
  drawRuler,
  maxTime,
  nearestEdge,
  panWindow,
  sliceBits,
  TRACK_H,
  valueAt,
  valueAtMarker,
  xToTime,
  zoomWindow,
  type DisplayUnit,
  type Markers,
  type Radix,
  type TimeWindow,
  type WaveTrace,
} from "./wave";

const $ = (id: string) => document.getElementById(id)!;
const SVGNS = "http://www.w3.org/2000/svg";

// A saved viewport: zoom factor + scroll offsets, remembered per scope so
// breadcrumb-back restores the view you left rather than re-fitting.
interface ViewState {
  k: number;
  scrollLeft: number;
  scrollTop: number;
}

interface ScopeFrame {
  path: string;
  label: string;
}

const state = {
  graph: null as SchematicGraph | null,
  stack: [] as ScopeFrame[],
  selected: null as number | null,
  source: new Map<number, string[]>(),
  // Signals pinned to the waveform pane, in lane order (top → bottom). Appended via
  // the schematic/source right-click menu; reordered/removed via the per-lane
  // controls. Cleared on model reload (#15).
  waves: [] as WaveTrace[],
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
      $("status").textContent = "elaborating…";
      top = await api.elaborateAndLoad(
        filelist,
        topName,
        incdirs,
        trace,
        ["TOP", "tb", "soc_pkg"],
        srcRoot,
      );
    } else {
      top = await api.loadDesign(model, trace, ["TOP", "tb", "soc_pkg"], srcRoot);
    }
    $("status").textContent = `loaded ${top}`;
    state.stack = [];
    viewCache.clear();
    // A new design invalidates the old traces (signal_refs are model-specific).
    state.waves = [];
    state.waveView = null;
    state.markers = { a: null, b: null };
    state.timescale = await api.traceTimescale();
    state.waveUnit = defaultDisplayUnit(state.timescale);
    syncUnitSelect();
    renderWaves();
    await initHierarchy(top);
    await setScope(top, top);
  } catch (e) {
    $("status").textContent = `error: ${e}`;
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

// Rendered tree rows keyed by scope path, for selection-highlight sync.
const treeItems = new Map<string, HTMLElement>();

// Build (or rebuild, on model reload) the tree pane: the design top plus its
// direct children; deeper levels are fetched lazily on expand.
async function initHierarchy(top: string) {
  const host = $("hierarchy");
  host.innerHTML = "";
  treeItems.clear();
  const root = await api.hierarchyTree(top, 1);
  const ul = document.createElement("ul");
  ul.appendChild(treeItem(root));
  host.appendChild(ul);
  highlightTree(top);
}

function treeItem(node: TreeNode): HTMLLIElement {
  const li = document.createElement("li");
  const row = document.createElement("div");
  row.className = "tnode";
  const twist = document.createElement("span");
  twist.className = "twist";
  const label = document.createElement("span");
  label.className = "tlabel";
  label.textContent = node.label;
  row.appendChild(twist);
  row.appendChild(label);
  // Module sublabel, unless it repeats the label (e.g. the design top).
  if (node.module && node.module !== node.label) {
    const mod = document.createElement("span");
    mod.className = "tmod";
    mod.textContent = `(${node.module})`;
    row.appendChild(mod);
  }
  li.appendChild(row);
  treeItems.set(node.path, row);

  let kids: HTMLUListElement | null = null;
  const attachKids = (children: TreeNode[]) => {
    const ul = document.createElement("ul");
    for (const c of children) ul.appendChild(treeItem(c));
    li.appendChild(ul);
    kids = ul;
    return ul;
  };
  if (node.children.length) attachKids(node.children);

  let open = node.children.length > 0;
  const setOpen = (o: boolean) => {
    open = o;
    twist.textContent = node.expandable ? (open ? "▾" : "▸") : "";
    if (kids) kids.style.display = open ? "" : "none";
  };
  setOpen(open);
  twist.onclick = async (e) => {
    e.stopPropagation();
    if (!node.expandable) return;
    if (!kids) {
      // Lazy: fetch this node's direct children on first expand. Reserve the
      // list synchronously so a second click during the fetch can't attach a
      // duplicate one.
      const ul = attachKids([]);
      const sub = await api.hierarchyTree(node.path, 1);
      for (const c of sub.children) ul.appendChild(treeItem(c));
    }
    setOpen(!open);
  };
  // Clicking the row navigates the schematic to this scope.
  row.onclick = () => jumpToScope(node.path);
  return li;
}

// Navigate to an arbitrary scope from the tree: the breadcrumb becomes the
// scope's ancestor chain (not the drill-down history), then the schematic
// loads it like any other navigation.
function jumpToScope(path: string) {
  rememberCurrentView();
  const frames = scopeFrames(path);
  state.stack = frames.slice(0, -1);
  setScope(path, frames[frames.length - 1].label);
}

// Keep the tree's selection in step with whatever sets the scope (tree click,
// schematic drill, breadcrumb). A scope deeper than the fetched tree has no
// row yet — the highlight simply clears until that branch is expanded.
function highlightTree(path: string) {
  for (const [p, el] of treeItems) el.classList.toggle("sel", p === path);
}

// -- schematic -------------------------------------------------------------

// Zoom bounds shared by manual zoom (setZoom) and explicit fit (fitView, #114).
const MIN_ZOOM = 0.2;
const MAX_ZOOM = 6;

// Current zoom factor. Manual zoom (setZoom) mutates it in place; a scope change
// resets it via renderSchematic's zoom-to-fit (or restores a saved view on back).
const zoom = { k: 1 };

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
  // Left-click a wire: highlight the net and show it in source. Right-click:
  // highlight + open the action menu (append to waveform / show in source).
  // Shared by the routed edges and the trunk fan-out geometry below. A member
  // stub passes its trunk's bundle path so selecting the member also lights
  // the trunk it hangs off (but never its sibling stubs).
  const wireHandlers = (netPath: string | undefined, trunkPath?: string) =>
    netPath
      ? {
          left: (ev: Event) => {
            ev.preventDefault();
            selectWire(netPath, trunkPath);
            api
              .probeNode(netPath, context())
              .then((r) => r && showInSource(r))
              .catch((e) => ($("status").textContent = `error: ${e}`));
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
// cross-probes; an interface is a leaf bundle, so there is no drill.
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
  name.textContent = inst;
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

// Right-click action menu for a schematic object: resolve it once, then offer
// "Append to waveform" (when the object has a trace signal) and "Show in source"
// (when it has a source location). Disabled items annotate why.
async function schematicMenu(ev: MouseEvent, path: string) {
  let resp: ProbeResponse | null;
  try {
    resp = await api.probeNode(path, context());
  } catch (e) {
    $("status").textContent = `error: ${e}`;
    return;
  }
  if (!resp) return;
  openContextMenu(ev.clientX, ev.clientY, [
    {
      label: resp.wave.in_trace
        ? "Append to waveform"
        : "Append to waveform (not in trace)",
      enabled: resp.wave.in_trace,
      onClick: () => addToWaveform(resp.wave),
    },
    {
      label: resp.source ? "Show in source" : "Show in source (no location)",
      enabled: !!resp.source,
      onClick: () => showInSource(resp),
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

// Show a cross-probe result in the source pane (jump to its location, if any) and
// list any ambiguous alternatives. Waveform display is now an explicit, additive
// action (the menu's "Append to waveform"), so it is no longer touched here.
async function showInSource(resp: ProbeResponse) {
  if (resp.source) await renderSource(resp.source.file, resp.source.line);
  renderPicker(resp);
}

async function renderSource(file: number, line: number) {
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

  const host = $("source");
  host.innerHTML = "";
  lines.forEach((text, i) => {
    const div = document.createElement("div");
    div.className = "line" + (i + 1 === line ? " hl" : "");
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
  return state.waveView ?? { t0: 0, t1: maxTime(state.waves) };
}

// Rebuild the waveform pane: a top ruler row, then one grid row per pinned signal —
// name | value-at-A | track | reorder (↑/↓) + remove (×). Track/ruler cells are
// canvases drawn by `redrawTracks` on the shared visible window; the grid keeps the
// time columns aligned across rows (#15, #16).
function renderWaves() {
  const list = $("wave-list");
  list.innerHTML = "";
  if (!state.waves.length) {
    list.classList.remove("has-rows");
    list.textContent = "(no signals)";
    updateMarkerReadout();
    return;
  }
  list.classList.add("has-rows");
  // Ruler row: spacers flank a track-column canvas so it aligns with the tracks.
  const rulerCell = document.createElement("div");
  rulerCell.className = "wave-ruler-cell";
  const ruler = document.createElement("canvas");
  ruler.className = "wave-ruler";
  rulerCell.appendChild(ruler);
  list.append(spacer(), spacer(), rulerCell, spacer());

  state.waves.forEach((tr, i) => {
    const name = document.createElement("div");
    name.className = "wave-row-name";
    name.textContent = tr.name;
    name.title = tr.name;
    // Right-click the name (not the track — that drops marker B) for per-signal
    // value formatting: change radix / create a sub-bus (#78).
    name.oncontextmenu = (e) => {
      e.preventDefault();
      openSignalMenu(e, i);
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
    ctrls.appendChild(waveBtn("↑", i === 0, () => moveWave(i, -1)));
    ctrls.appendChild(waveBtn("↓", i === state.waves.length - 1, () => moveWave(i, 1)));
    ctrls.appendChild(waveBtn("×", false, () => removeWave(i)));

    list.append(name, value, cell, ctrls);
  });
  redrawTracks();
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
  state.waves.forEach((tr, i) => {
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

function moveWave(i: number, dir: number) {
  const j = i + dir;
  if (j < 0 || j >= state.waves.length) return;
  [state.waves[i], state.waves[j]] = [state.waves[j], state.waves[i]];
  renderWaves();
}

function removeWave(i: number) {
  state.waves.splice(i, 1);
  renderWaves();
}

const RADIX_LABELS: { r: Radix; label: string }[] = [
  { r: "hex", label: "Hex" },
  { r: "dec", label: "Decimal" },
  { r: "oct", label: "Octal" },
  { r: "bin", label: "Binary" },
];

// Derived sub-bus tracks get unique negative refs so they never collide with real
// u32 signal_refs (dedup/reorder/remove stay correct).
let derivedSeq = -1;

// Bit width of a trace when it is a sliceable bit-vector (binary value string), else 0.
function busWidth(tr: WaveTrace): number {
  const v = tr.values.find((c) => c.value.length > 0);
  return v && /^[01xz]+$/i.test(v.value) ? v.value.length : 0;
}

// Per-signal value-format menu (radix + sub-bus), opened from the name cell (#78).
function openSignalMenu(ev: MouseEvent, i: number) {
  const tr = state.waves[i];
  if (!tr) return;
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
  const width = busWidth(tr);
  openContextMenu(ev.clientX, ev.clientY, [
    { label: "Change radix", enabled: true, submenu: radixSubmenu },
    {
      label: width > 1 ? "Create sub-bus…" : "Create sub-bus… (not a bus)",
      enabled: width > 1,
      onClick: () => openSubBusPopover(ev, i, width),
    },
  ]);
}

// Insert a derived track of parent[hi:lo] right after the parent.
function makeSubBus(i: number, hi: number, lo: number) {
  const tr = state.waves[i];
  if (!tr) return;
  const values = tr.values.map((c) => ({ time: c.time, value: sliceBits(c.value, hi, lo) }));
  state.waves.splice(i + 1, 0, {
    ref: derivedSeq--,
    name: `${tr.name}[${hi}:${lo}]`,
    values,
    radix: tr.radix ?? "hex",
  });
  renderWaves();
}

// Small inline popover to pick the [hi:lo] bit range for a sub-bus.
function openSubBusPopover(ev: MouseEvent, i: number, width: number) {
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
    makeSubBus(i, hi, lo);
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
    d.onclick = () => api.probeNode(alt.path, context()).then((r) => r && showInSource(r));
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
      onClick: () => showInSchematic(resp.anchor),
    },
    {
      label: resp.wave.in_trace
        ? "Append to waveform"
        : "Append to waveform (not in trace)",
      enabled: resp.wave.in_trace,
      onClick: () => addToWaveform(resp.wave),
    },
  ]);
}

// Breadcrumb frames for every ancestor of a scope path. All ancestors of a
// navigable scope (Instance/GenBlock) are themselves navigable, so each frame is
// a valid drill target.
function framesForScope(path: string): ScopeFrame[] {
  const out: ScopeFrame[] = [];
  let acc = "";
  for (const seg of path.split(".")) {
    acc = acc ? `${acc}.${seg}` : seg;
    out.push({ path: acc, label: seg });
  }
  return out;
}

// Navigate the schematic to show `anchor`: drill into it if it is itself a scope,
// else open the nearest enclosing scope and highlight the box/wire it maps to.
async function showInSchematic(anchor: NodeRef) {
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
    state.stack = framesForScope(scopePath);
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

// Append the signal as a new waveform lane (deduped by trace ref); a no-op when the
// object has no trace signal. Lanes stack in append order (#15).
async function addToWaveform(wave: WaveLink) {
  if (!wave.in_trace) return;
  if (state.waves.some((w) => w.ref === wave.signal_ref)) return;
  const values = await api.signalValues(wave.signal_ref);
  // Enum-typed signals carry a value→name map; show the state name by default.
  const enumMap = wave.enum_map
    ? new Map(wave.enum_map.map((m) => [m.value, m.name]))
    : undefined;
  state.waves.push({
    ref: wave.signal_ref,
    name: wave.full_name,
    values,
    radix: "hex",
    enumMap,
    showName: enumMap !== undefined,
  });
  renderWaves();
}

// -- bootstrap -------------------------------------------------------------

// Dark is the default; the toggle flips to a light schematic theme and persists.
function initTheme() {
  try {
    if (localStorage.getItem("theme") === "light") {
      document.documentElement.dataset.theme = "light";
    }
  } catch {
    /* localStorage may be unavailable; default dark is fine */
  }
  $("theme-toggle").addEventListener("click", () => {
    const root = document.documentElement;
    const toLight = root.dataset.theme !== "light";
    if (toLight) root.dataset.theme = "light";
    else delete root.dataset.theme;
    try {
      localStorage.setItem("theme", toLight ? "light" : "dark");
    } catch {
      /* ignore persistence failure */
    }
  });
}

// Waveform zoom / pan / marker interaction. Listeners are delegated on #wave-list so
// they survive the per-change canvas rebuilds. Pixel→time uses each canvas's laid-out
// rect (drawing width == clientWidth, so 1 CSS px == 1 device px — see redrawTracks).
function setupWaveInteraction() {
  const list = $("wave-list");
  const tMax = () => maxTime(state.waves);

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
    let candidates = state.waves;
    if (canvas.classList.contains("wave-track")) {
      const idx = Array.from(list.querySelectorAll(".wave-track")).indexOf(canvas);
      if (idx >= 0 && state.waves[idx]) candidates = [state.waves[idx]];
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

async function init() {
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
  initTheme();
  setupZoom();
  setupWaveInteraction();
  syncUnitSelect();
  loadColWidths();
  loadRowSplit();
  setupRowSplitter();
  renderWaves(); // show the empty-state "(no signals)" list until a trace is added
  // Source right-click menu (#19), and dismissals.
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
