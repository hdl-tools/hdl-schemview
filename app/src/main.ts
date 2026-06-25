// hdl-schemview frontend: three panes (schematic / source / waveform) linked by
// one selection, resolved through the cross-probe commands.
import { api } from "./api";
import { layout, nodeId } from "./elk";
import type { ProbeResponse, SchematicGraph, SchNode, SchPort, ValueChange } from "./types";

const $ = (id: string) => document.getElementById(id)!;
const SVGNS = "http://www.w3.org/2000/svg";

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
    await setScope(top, top);
  } catch (e) {
    $("status").textContent = `error: ${e}`;
  }
}

async function setScope(path: string, label: string, push = true) {
  const graph = await api.scopeGraph(path);
  state.graph = graph;
  if (push) state.stack.push({ path, label });
  renderBreadcrumb();
  await renderSchematic(graph);
}

function renderBreadcrumb() {
  const bc = $("breadcrumb");
  bc.innerHTML = "";
  state.stack.forEach((f, i) => {
    const s = document.createElement("span");
    s.textContent = f.label;
    s.onclick = () => {
      state.stack = state.stack.slice(0, i);
      setScope(f.path, f.label);
    };
    bc.appendChild(s);
    if (i < state.stack.length - 1) bc.appendChild(document.createTextNode(" / "));
  });
}

// -- schematic -------------------------------------------------------------

// Current zoom factor; persists across re-renders so selection/expansion keeps it.
const zoom = { k: 1 };

async function renderSchematic(graph: SchematicGraph) {
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

  // 1. Wires (under everything). Collect each net's label to draw last, on top,
  //    attached to the midpoint of the wire's longest (most legible) segment.
  //    A net name is drawn only once per view (it may fan out over many wires).
  const wireLabels: SVGTextElement[] = [];
  const seenNets = new Set<string>();
  for (const e of laid.edges ?? []) {
    const segs: [any, any][] = [];
    for (const sec of e.sections ?? []) {
      const pts = [sec.startPoint, ...(sec.bendPoints ?? []), sec.endPoint];
      const path = document.createElementNS(SVGNS, "polyline");
      path.setAttribute("class", "wire");
      path.setAttribute("points", pts.map((p: any) => `${p.x},${p.y}`).join(" "));
      root.appendChild(path);
      for (let i = 0; i < pts.length - 1; i++) segs.push([pts[i], pts[i + 1]]);
    }
    const text = e.labels?.[0]?.text;
    if (text && segs.length && !seenNets.has(text)) {
      seenNets.add(text);
      let best = segs[0];
      let bestLen = -1;
      for (const [a, b] of segs) {
        const len = Math.abs(a.x - b.x) + Math.abs(a.y - b.y);
        if (len > bestLen) [bestLen, best] = [len, [a, b]];
      }
      const [a, b] = best;
      const horizontal = Math.abs(a.x - b.x) >= Math.abs(a.y - b.y);
      const t = document.createElementNS(SVGNS, "text");
      t.setAttribute("class", "wire-label");
      t.setAttribute("x", String((a.x + b.x) / 2));
      t.setAttribute("y", String((a.y + b.y) / 2 + (horizontal ? -3 : 0)));
      t.setAttribute("text-anchor", "middle");
      if (!horizontal) t.setAttribute("dominant-baseline", "middle");
      t.textContent = text;
      wireLabels.push(t);
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

    const portById = new Map<number, SchPort>();
    node?.ports.forEach((p) => portById.set(p.id, p));

    const g = document.createElementNS(SVGNS, "g");
    g.setAttribute("transform", `translate(${c.x},${c.y})`);

    const rect = document.createElementNS(SVGNS, "rect");
    rect.setAttribute("class", "box" + (state.selected === id ? " sel" : ""));
    rect.setAttribute("width", String(c.width));
    rect.setAttribute("height", String(c.height));
    rect.setAttribute("rx", "4");
    rect.onclick = () => selectNode(id);
    rect.ondblclick = () => {
      if (node?.expandable) setScope(node.path ?? "", node.label);
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

    // Pins: a direction arrow drawn *just inside* the border so the wire lands
    // exactly on the boundary (in on the west, out on the east), plus the port
    // name + bit-width clear of the arrow.
    for (const p of c.ports ?? []) {
      const pid = Number(String(p.id).slice(1));
      const sp = portById.get(pid);
      const px = p.x ?? 0;
      const py = p.y ?? 0;
      const west = sp ? sp.side !== "east" : px < c.width / 2;

      const arrow = document.createElementNS(SVGNS, "path");
      arrow.setAttribute("class", "pin " + (west ? "pin-in" : "pin-out"));
      // Triangle centred on the port point (px,py) so the wire connects to its
      // centre; signal flows L→R, so it always points right.
      arrow.setAttribute("d", `M${px - 3},${py - 4} L${px - 3},${py + 4} L${px + 5},${py} Z`);
      arrow.onclick = () => selectNode(pid);
      g.appendChild(arrow);

      if (sp) {
        const t = document.createElementNS(SVGNS, "text");
        t.setAttribute("class", "pin-label");
        t.setAttribute("x", String(west ? px + 11 : px - 11));
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
  for (const t of wireLabels) root.appendChild(t);

  host.appendChild(svg);
  applyZoom(svg);
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
  // Triangle centred on the port point so the wire connects to its centre.
  arrow.setAttribute("d", `M${px - 3},${py - 4} L${px - 3},${py + 4} L${px + 5},${py} Z`);
  if (!isConst) arrow.onclick = () => selectNode(id);
  g.appendChild(arrow);

  const t = document.createElementNS(SVGNS, "text");
  t.setAttribute("class", isConst ? "const-label" : "pin-label");
  t.setAttribute("x", String(input ? px - 12 : px + 12));
  t.setAttribute("y", String(py + 3));
  t.setAttribute("text-anchor", input ? "end" : "start");
  t.textContent = sp?.width ? `${sp.name}${sp.width}` : (sp?.name ?? node.label);
  if (!isConst) t.onclick = () => selectNode(id);
  g.appendChild(t);

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
}

// Ctrl/⌘ + wheel zooms toward the cursor; plain wheel keeps scrolling.
function setupZoom() {
  const host = $("schematic");
  host.addEventListener(
    "wheel",
    (ev) => {
      if (!ev.ctrlKey && !ev.metaKey) return;
      ev.preventDefault();
      setZoom(zoom.k * (ev.deltaY < 0 ? 1.15 : 1 / 1.15), { x: ev.clientX, y: ev.clientY });
    },
    { passive: false },
  );
  $("zoom-in").addEventListener("click", () => setZoom(zoom.k * 1.25));
  $("zoom-out").addEventListener("click", () => setZoom(zoom.k / 1.25));
  $("zoom-reset").addEventListener("click", () => setZoom(1));
}

// `node.path` isn't in the layout; look it up from the graph by id.
function pathOf(id: number): string | null {
  return state.graph?.nodes.find((n) => n.id === id)?.path ?? null;
}

async function selectNode(id: number) {
  state.selected = id;
  const path = pathOf(id);
  if (!path) return;
  const resp = await api.probeNode(path, context());
  if (resp) applyProbe(resp);
  if (state.graph) await renderSchematic(state.graph);
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
    lines = text.split("\n");
    state.source.set(file, lines);
  }
  const host = $("source");
  host.innerHTML = "";
  lines.forEach((text, i) => {
    const div = document.createElement("div");
    div.className = "line" + (i + 1 === line ? " hl" : "");
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
}

document.addEventListener("DOMContentLoaded", init);

// Keep nodeId referenced for potential external callers / tree-shaking clarity.
export { nodeId };
