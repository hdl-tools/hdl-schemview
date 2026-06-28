// hdl-schemview frontend: three panes (schematic / source / waveform) linked by
// one selection, resolved through the cross-probe commands.
import { api } from "./api";
import {
  clampSegmentToRect,
  ffRole,
  fitZoom,
  isLogicKind,
  layout,
  nodeId,
  wireLabelPlacement,
} from "./elk";
import type { Pt } from "./elk";
import type {
  NodeRef,
  ProbeResponse,
  SchematicGraph,
  SchNode,
  SchPort,
  ValueChange,
  WaveLink,
} from "./types";

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

async function load() {
  const model = (($("model") as HTMLInputElement).value || "").trim();
  const trace = (($("trace") as HTMLInputElement).value || "").trim();
  const srcRoot = (($("srcroot") as HTMLInputElement).value || ".").trim();
  try {
    const top = await api.loadDesign(model, trace, ["TOP", "tb", "soc_pkg"], srcRoot);
    $("status").textContent = `loaded ${top}`;
    state.stack = [];
    viewCache.clear();
    await setScope(top, top);
  } catch (e) {
    $("status").textContent = `error: ${e}`;
  }
}

// A scope with a cached viewport is restored to it; a first-time scope is
// zoom-to-fit and scrolled top-left (see renderSchematic).
async function setScope(path: string, label: string, push = true) {
  const graph = await api.scopeGraph(path);
  state.graph = graph;
  if (push) state.stack.push({ path, label });
  renderBreadcrumb();
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

// -- schematic -------------------------------------------------------------

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
  for (const e of laid.edges ?? []) {
    const sch = edgeById.get(Number(String(e.id).slice(1)));
    const netPath = sch?.net_path;
    // Cross-probe the net to source + waveform; usable as both a left-click and a
    // right-click handler (a wire has no drill/double-click, so either is safe).
    const probeWire = netPath
      ? (ev: Event) => {
          ev.preventDefault();
          selectWire(netPath);
          crossProbePath(netPath);
        }
      : null;
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
      if (probeWire) {
        const hit = document.createElementNS(SVGNS, "polyline");
        hit.setAttribute("class", "wire-hit");
        hit.setAttribute("points", points);
        hit.onclick = probeWire;
        hit.oncontextmenu = probeWire;
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
        if (probeWire && netPath) {
          t.classList.add("clickable");
          t.dataset.netPath = netPath;
          t.onclick = probeWire;
          t.oncontextmenu = probeWire;
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

  // 2. Boxes.
  for (const c of laid.children ?? []) {
    const id = Number(String(c.id).slice(1));
    const node = graph.nodes.find((n) => n.id === id);

    // Boundary I/O pin (the scope's own port): a frame pin + label, not a box.
    if (node?.kind === "Port") {
      renderBoundaryPin(root, c, node, id);
      continue;
    }
    // Inferred register: a generic flip-flop symbol.
    if (node?.kind === "FF") {
      renderFF(root, c, node, id);
      continue;
    }
    // Continuous assign: a stadium (rounded-end capsule) function node.
    if (node?.kind === "Assign") {
      renderAssign(root, c, node, id);
      continue;
    }
    // Level-sensitive latch: a storage box with a level-enable marker.
    if (node?.kind === "Latch") {
      renderLatch(root, c, node, id);
      continue;
    }
    // SystemVerilog interface instance / interface port: a folded-corner bundle.
    if (node?.kind === "Interface") {
      renderInterface(root, c, node, id);
      continue;
    }

    const portById = new Map<number, SchPort>();
    node?.ports.forEach((p) => portById.set(p.id, p));

    const g = document.createElementNS(SVGNS, "g");
    g.setAttribute("transform", `translate(${c.x},${c.y})`);

    const rect = document.createElementNS(SVGNS, "rect");
    // Logic kinds (comb/latch) get a per-kind class so combinational processes read
    // apart from module-instance boxes by colour. (Assign has its own shape — see
    // renderAssign — so it never reaches this generic path.)
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
      crossProbe(id);
    };
    g.appendChild(rect);

    // Title: instance name, with the module type on a second line (like a
    // schematic block caption), centred in the box.
    const cx = c.width / 2;
    const cy = c.height / 2;
    const name = document.createElementNS(SVGNS, "text");
    name.setAttribute("class", "box-label");
    name.setAttribute("x", String(cx));
    name.setAttribute("y", String(node?.module ? cy - 4 : cy + 4));
    name.setAttribute("text-anchor", "middle");
    name.textContent = (c.labels?.[0]?.text ?? "") + (node?.expandable ? " ▸" : "");
    name.style.pointerEvents = "none";
    g.appendChild(name);
    if (node?.module) {
      const mod = document.createElementNS(SVGNS, "text");
      mod.setAttribute("class", "box-sublabel");
      mod.setAttribute("x", String(cx));
      mod.setAttribute("y", String(cy + 12));
      mod.setAttribute("text-anchor", "middle");
      mod.textContent = `(${node.module})`;
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
      arrow.setAttribute("class", "pin " + (west ? "pin-in" : "pin-out"));
      arrow.setAttribute(
        "d",
        west
          ? `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX + PIN},${py} Z`
          : `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX - PIN},${py} Z`,
      );
      arrow.onclick = () => selectNode(pid);
      g.appendChild(arrow);

      // A logic node (comb/latch/assign) is a process, not a module: its pins are
      // bare wire stubs, so skip the per-pin signal-name labels (the wire already
      // carries that name).
      if (sp && !isLogicKind(node?.kind ?? "")) {
        const t = document.createElementNS(SVGNS, "text");
        t.setAttribute("class", "pin-label");
        t.setAttribute("x", String(west ? edgeX + LABEL_PAD : edgeX - LABEL_PAD));
        t.setAttribute("y", String(py + 3));
        t.setAttribute("text-anchor", west ? "start" : "end");
        t.textContent = sp.width ? `${sp.name}${sp.width}` : sp.name;
        t.onclick = () => selectNode(pid);
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
  const probePin = (ev: Event) => {
    ev.preventDefault();
    crossProbe(id);
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

// An inferred register, drawn as a generic flip-flop: a plain square labelled
// "FF" with a clock wedge (clk, on the left wall), an active-low bubble (reset,
// outer bottom), conditions along the bottom, and Q at the right centre. Pin
// positions come from ELK (FIXED_POS in `ffChild`); here we add the glyphs.
function renderFF(parent: SVGElement, c: any, node: SchNode, id: number) {
  const W = c.width;
  const H = c.height;
  const g = document.createElementNS(SVGNS, "g");
  g.setAttribute("transform", `translate(${c.x},${c.y})`);

  const rect = document.createElementNS(SVGNS, "rect");
  rect.setAttribute("class", "box ff" + (state.selected === id ? " sel" : ""));
  rect.setAttribute("width", String(W));
  rect.setAttribute("height", String(H));
  rect.setAttribute("rx", "3");
  rect.dataset.nodeId = String(id);
  rect.onclick = () => selectNode(id);
  rect.oncontextmenu = (e) => {
    e.preventDefault();
    crossProbe(id);
  };
  g.appendChild(rect);

  const t = document.createElementNS(SVGNS, "text");
  t.setAttribute("class", "box-label");
  t.setAttribute("x", String(W / 2));
  t.setAttribute("y", String(H / 2 + 1));
  t.setAttribute("text-anchor", "middle");
  t.style.pointerEvents = "none";
  t.textContent = "FF";
  g.appendChild(t);

  const portById = new Map<number, SchPort>();
  node.ports.forEach((p) => portById.set(p.id, p));
  for (const p of c.ports ?? []) {
    const sp = portById.get(Number(String(p.id).slice(1)));
    if (!sp) continue;
    const px = p.x ?? 0;
    const role = ffRole(sp);
    if (role === "clk") {
      // Clock-edge wedge on the left wall: base flush to the wall, apex pointing
      // right into the box.
      const py = p.y ?? 0;
      const tri = document.createElementNS(SVGNS, "path");
      tri.setAttribute("class", "ff-clk");
      tri.setAttribute("d", `M0,${py - 6} L0,${py + 6} L10,${py} Z`);
      g.appendChild(tri);
    } else if (role === "reset") {
      // Active-low reset bubble, centred on and just below the bottom edge.
      const circ = document.createElementNS(SVGNS, "circle");
      circ.setAttribute("class", "ff-rst");
      circ.setAttribute("cx", String(px));
      circ.setAttribute("cy", String(H + 3));
      circ.setAttribute("r", "3");
      g.appendChild(circ);
    } else if (role === "q") {
      // One output stub per distinct output, so a register driving several
      // signals shows each output individually (base on the east wall, apex in).
      const py = p.y ?? 0;
      const tri = document.createElementNS(SVGNS, "path");
      tri.setAttribute("class", "pin pin-out");
      tri.setAttribute("d", `M${W},${py - 4} L${W},${py + 4} L${W - 8},${py} Z`);
      g.appendChild(tri);
    }
  }
  parent.appendChild(g);
}

// A level-sensitive latch: a storage rectangle (like the FF) but transparent on an
// active level rather than a clock edge. Distinguished from the FF by its "LE"
// caption (the FF carries an edge wedge instead) and from the comb rectangle by its
// own tint. Pins are bare stubs like the other logic nodes.
function renderLatch(parent: SVGElement, c: any, node: SchNode, id: number) {
  const W = c.width;
  const H = c.height;
  const g = document.createElementNS(SVGNS, "g");
  g.setAttribute("transform", `translate(${c.x},${c.y})`);

  const rect = document.createElementNS(SVGNS, "rect");
  rect.setAttribute("class", "box latch" + (state.selected === id ? " sel" : ""));
  rect.setAttribute("width", String(W));
  rect.setAttribute("height", String(H));
  rect.setAttribute("rx", "3");
  rect.dataset.nodeId = String(id);
  rect.onclick = () => selectNode(id);
  rect.oncontextmenu = (e) => {
    e.preventDefault();
    crossProbe(id);
  };
  g.appendChild(rect);

  const t = document.createElementNS(SVGNS, "text");
  t.setAttribute("class", "box-label");
  t.setAttribute("x", String(W / 2));
  t.setAttribute("y", String(H / 2 + 1));
  t.setAttribute("text-anchor", "middle");
  t.style.pointerEvents = "none";
  t.textContent = "LE";
  g.appendChild(t);

  // Bare pin stubs (no per-pin labels), like the other logic nodes.
  const PIN = 8;
  for (const p of c.ports ?? []) {
    const pid = Number(String(p.id).slice(1));
    const sp = node.ports.find((q) => q.id === pid);
    const py = p.y ?? 0;
    const west = sp ? sp.side !== "east" : (p.x ?? 0) < W / 2;
    const edgeX = west ? 0 : W;
    const arrow = document.createElementNS(SVGNS, "path");
    arrow.setAttribute("class", "pin " + (west ? "pin-in" : "pin-out"));
    arrow.setAttribute(
      "d",
      west
        ? `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX + PIN},${py} Z`
        : `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX - PIN},${py} Z`,
    );
    arrow.onclick = () => selectNode(pid);
    g.appendChild(arrow);
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
  const FOLD = 11; // dog-ear size
  const portById = new Map<number, SchPort>();
  node.ports.forEach((p) => portById.set(p.id, p));

  const g = document.createElementNS(SVGNS, "g");
  g.setAttribute("transform", `translate(${c.x},${c.y})`);

  // Body with the top-right corner cut away (the fold sits there instead).
  const body = document.createElementNS(SVGNS, "path");
  body.setAttribute("class", "box iface" + (state.selected === id ? " sel" : ""));
  body.setAttribute(
    "d",
    `M4,0 L${W - FOLD},0 L${W},${FOLD} L${W},${H - 4} Q${W},${H} ${W - 4},${H} ` +
      `L4,${H} Q0,${H} 0,${H - 4} L0,4 Q0,0 4,0 Z`,
  );
  body.dataset.nodeId = String(id);
  body.onclick = () => selectNode(id);
  body.oncontextmenu = (e) => {
    e.preventDefault();
    crossProbe(id);
  };
  g.appendChild(body);

  // The folded-over corner flap.
  const fold = document.createElementNS(SVGNS, "path");
  fold.setAttribute("class", "iface-fold");
  fold.setAttribute("d", `M${W - FOLD},0 L${W - FOLD},${FOLD} L${W},${FOLD} Z`);
  fold.style.pointerEvents = "none";
  g.appendChild(fold);

  const cx = W / 2;
  const cy = H / 2;
  const name = document.createElementNS(SVGNS, "text");
  name.setAttribute("class", "box-label");
  name.setAttribute("x", String(cx));
  name.setAttribute("y", String(node.module ? cy - 4 : cy + 4));
  name.setAttribute("text-anchor", "middle");
  name.textContent = c.labels?.[0]?.text ?? node.label;
  name.style.pointerEvents = "none";
  g.appendChild(name);
  if (node.module) {
    const mod = document.createElementNS(SVGNS, "text");
    mod.setAttribute("class", "box-sublabel");
    mod.setAttribute("x", String(cx));
    mod.setAttribute("y", String(cy + 12));
    mod.setAttribute("text-anchor", "middle");
    mod.textContent = `(${node.module})`;
    mod.style.pointerEvents = "none";
    g.appendChild(mod);
  }

  // Interface ports (e.g. clk), drawn like a module box's pins.
  const PIN = 8;
  const LABEL_PAD = 11;
  for (const p of c.ports ?? []) {
    const pid = Number(String(p.id).slice(1));
    const sp = portById.get(pid);
    const py = p.y ?? 0;
    const west = sp ? sp.side !== "east" : (p.x ?? 0) < W / 2;
    const edgeX = west ? 0 : W;
    const arrow = document.createElementNS(SVGNS, "path");
    arrow.setAttribute("class", "pin " + (west ? "pin-in" : "pin-out"));
    arrow.setAttribute(
      "d",
      west
        ? `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX + PIN},${py} Z`
        : `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX - PIN},${py} Z`,
    );
    arrow.onclick = () => selectNode(pid);
    g.appendChild(arrow);
    if (sp) {
      const t = document.createElementNS(SVGNS, "text");
      t.setAttribute("class", "pin-label");
      t.setAttribute("x", String(west ? edgeX + LABEL_PAD : edgeX - LABEL_PAD));
      t.setAttribute("y", String(py + 3));
      t.setAttribute("text-anchor", west ? "start" : "end");
      t.textContent = sp.width ? `${sp.name}${sp.width}` : sp.name;
      t.onclick = () => selectNode(pid);
      g.appendChild(t);
    }
  }
  parent.appendChild(g);
}

// A continuous assign, drawn as a stadium (rounded-end capsule): a combinational
// function reducing its inputs (west) to a single output (east). Distinct from the
// comb rectangle and the module box. Pins sit on the rounded edge. Pin labels are
// skipped (the wire carries the net name), like the other logic nodes.
function renderAssign(parent: SVGElement, c: any, node: SchNode, id: number) {
  const W = c.width;
  const H = c.height;
  const r = H / 2; // capsule end radius
  const g = document.createElementNS(SVGNS, "g");
  g.setAttribute("transform", `translate(${c.x},${c.y})`);

  const rect = document.createElementNS(SVGNS, "rect");
  rect.setAttribute("class", "box assign" + (state.selected === id ? " sel" : ""));
  rect.setAttribute("width", String(W));
  rect.setAttribute("height", String(H));
  rect.setAttribute("rx", String(r));
  rect.setAttribute("ry", String(r));
  rect.dataset.nodeId = String(id);
  rect.onclick = () => selectNode(id);
  rect.oncontextmenu = (e) => {
    e.preventDefault();
    crossProbe(id);
  };
  g.appendChild(rect);

  const t = document.createElementNS(SVGNS, "text");
  t.setAttribute("class", "box-label");
  t.setAttribute("x", String(W / 2));
  t.setAttribute("y", String(H / 2));
  t.setAttribute("text-anchor", "middle");
  t.setAttribute("dominant-baseline", "central");
  t.style.pointerEvents = "none";
  t.textContent = "assign";
  g.appendChild(t);

  const portById = new Map<number, SchPort>();
  node.ports.forEach((p) => portById.set(p.id, p));
  const PIN = 8; // triangle depth
  for (const p of c.ports ?? []) {
    const pid = Number(String(p.id).slice(1));
    const sp = portById.get(pid);
    if (!sp) continue;
    const py = p.y ?? H / 2;
    const west = sp.side !== "east";
    // Inset the pin to the capsule's curved edge at this height so the triangle
    // sits on the rounded wall rather than floating left/right of it.
    const dy = Math.min(Math.abs(py - r), r);
    const inset = r - Math.sqrt(Math.max(0, r * r - dy * dy));
    const edgeX = west ? inset : W - inset;
    const arrow = document.createElementNS(SVGNS, "path");
    arrow.setAttribute("class", "pin " + (west ? "pin-in" : "pin-out"));
    arrow.setAttribute(
      "d",
      west
        ? `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX + PIN},${py} Z`
        : `M${edgeX},${py - 4} L${edgeX},${py + 4} L${edgeX - PIN},${py} Z`,
    );
    arrow.onclick = () => selectNode(pid);
    g.appendChild(arrow);
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
  zoom.k = Math.min(6, Math.max(0.2, k));
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
  zoom.k = fitZoom(bw, bh, host.clientWidth, host.clientHeight);
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
  host.addEventListener("scroll", placeWireLabels, { passive: true });
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
// the DOM intact so any pending double-click target survives.
function applySelection() {
  const host = $("schematic");
  host.querySelectorAll(".box.sel").forEach((el) => el.classList.remove("sel"));
  if (state.selected != null) {
    host.querySelector(`[data-node-id="${state.selected}"]`)?.classList.add("sel");
  }
}

// Right-click a box/pin to cross-probe it to source + waveform. A polished
// drop-down menu is the later-stage enhancement; this keeps cross-probing
// reachable now that single-click is schematic-only (#47).
async function crossProbe(id: number) {
  const path = pathOf(id);
  if (path) await crossProbePath(path);
}

// Cross-probe a node by its canonical model path — a pure cross-probe lookup, no
// id detour. Used for wires (whose net carries a path, not a graph-node id).
async function crossProbePath(path: string) {
  const resp = await api.probeNode(path, context());
  if (resp) applyProbe(resp);
}

// Highlight every wire + label carrying `netPath` (the net just clicked), and
// clear it from the rest. Box selection (`.box.sel`) is independent and untouched.
function selectWire(netPath: string) {
  const host = $("schematic");
  host.querySelectorAll<SVGElement>(".wire, .wire-label").forEach((el) => {
    el.classList.toggle("sel", el.dataset.netPath === netPath);
  });
}

// -- apply a cross-probe result to source + waveform -----------------------

async function applyProbe(resp: ProbeResponse) {
  if (resp.source) await renderSource(resp.source.file, resp.source.line);
  if (resp.wave.in_trace) {
    $("wave-name").textContent = resp.wave.full_name;
    const values = await api.signalValues(resp.wave.signal_ref);
    renderWave(values);
  } else {
    $("wave-name").textContent = "(not in trace)";
    renderWave([]);
  }
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

function renderWave(values: ValueChange[]) {
  const canvas = $("wave") as HTMLCanvasElement;
  const ctx = canvas.getContext("2d")!;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  if (!values.length) return;
  const tMax = values[values.length - 1].time || 1;
  const x = (t: number) => 10 + (t / tMax) * (canvas.width - 20);
  const digital = values.every((v) => v.value.length <= 1);
  ctx.strokeStyle = "#7fd";
  ctx.fillStyle = "#9cf";
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  const yHi = 30,
    yLo = 120,
    yMid = 75;
  let prevX = x(values[0].time);
  let prevY = digital ? (values[0].value === "1" ? yHi : yLo) : yMid;
  ctx.moveTo(prevX, prevY);
  for (const v of values) {
    const cx = x(v.time);
    if (digital) {
      const y = v.value === "1" ? yHi : yLo;
      ctx.lineTo(cx, prevY);
      ctx.lineTo(cx, y);
      prevY = y;
    } else {
      ctx.lineTo(cx, yMid);
      ctx.fillText(v.value, cx + 2, yMid - 4);
    }
    prevX = cx;
  }
  ctx.lineTo(canvas.width - 10, prevY);
  ctx.stroke();
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
    d.onclick = () => api.probeNode(alt.path, context()).then((r) => r && applyProbe(r));
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
}

interface MenuItem {
  label: string;
  enabled: boolean;
  onClick: () => void;
}

function openContextMenu(x: number, y: number, items: MenuItem[]) {
  const menu = $("ctxmenu");
  menu.innerHTML = "";
  for (const item of items) {
    const d = document.createElement("div");
    d.className = "ctx-item" + (item.enabled ? "" : " disabled");
    d.textContent = item.label;
    if (item.enabled) {
      d.onclick = () => {
        closeContextMenu();
        item.onClick();
      };
    }
    menu.appendChild(d);
  }
  menu.style.left = `${x}px`;
  menu.style.top = `${y}px`;
  menu.style.display = "block";
}

// Right-click in the source pane: resolve the signal/object under the cursor and
// offer "Show in schematic" / "Add to waveform" (the latter disabled when the
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
      label: resp.wave.in_trace ? "Add to waveform" : "Add to waveform (not in trace)",
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
    // highlight.
    if (scopePath !== anchor.path) {
      selectNode(anchor.id);
      selectWire(anchor.path);
      const host = $("schematic");
      const el =
        host.querySelector<SVGGraphicsElement>(`[data-node-id="${anchor.id}"]`) ??
        host.querySelector<SVGGraphicsElement>(".wire.sel");
      el?.scrollIntoView({ block: "center", inline: "center" });
    }
    return;
  }
}

// Show the signal in the waveform pane. Single-trace for now; becomes additive
// once the multi-signal viewer lands (#15).
async function addToWaveform(wave: WaveLink) {
  if (!wave.in_trace) return;
  $("wave-name").textContent = wave.full_name;
  const values = await api.signalValues(wave.signal_ref);
  renderWave(values);
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

function init() {
  ($("model") as HTMLInputElement).value =
    "../../fixtures/picorv32_soc/golden/hierarchy.json";
  ($("trace") as HTMLInputElement).value =
    "../../fixtures/picorv32_soc/traces/picorv32_soc.fst";
  ($("srcroot") as HTMLInputElement).value = "../..";
  $("load").addEventListener("click", load);
  initTheme();
  setupZoom();
  // Source right-click menu (#19), and dismissals.
  $("source").addEventListener("contextmenu", onSourceContextMenu);
  document.addEventListener("click", closeContextMenu);
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeContextMenu();
  });
}

document.addEventListener("DOMContentLoaded", init);

// Keep nodeId referenced for potential external callers / tree-shaking clarity.
export { nodeId };
