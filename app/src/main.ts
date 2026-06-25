// hdl-schemview frontend: three panes (schematic / source / waveform) linked by
// one selection, resolved through the cross-probe commands.
import { api } from "./api";
import { layout, nodeId } from "./elk";
import type { ProbeResponse, SchematicGraph, ValueChange } from "./types";

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

async function renderSchematic(graph: SchematicGraph) {
  const host = $("schematic");
  host.innerHTML = "";
  if (!graph.nodes.length) {
    host.textContent = "(empty scope)";
    return;
  }
  const laid: any = await layout(graph);
  const svg = document.createElementNS(SVGNS, "svg");
  svg.setAttribute("width", String(Math.max(laid.width ?? 400, 200)));
  svg.setAttribute("height", String(Math.max(laid.height ?? 300, 150)));

  for (const e of laid.edges ?? []) {
    for (const sec of e.sections ?? []) {
      const pts = [sec.startPoint, ...(sec.bendPoints ?? []), sec.endPoint];
      const path = document.createElementNS(SVGNS, "polyline");
      path.setAttribute("class", "wire");
      path.setAttribute("points", pts.map((p: any) => `${p.x},${p.y}`).join(" "));
      svg.appendChild(path);
    }
  }

  for (const c of laid.children ?? []) {
    const id = Number(String(c.id).slice(1));
    const g = document.createElementNS(SVGNS, "g");
    g.setAttribute("transform", `translate(${c.x},${c.y})`);

    const rect = document.createElementNS(SVGNS, "rect");
    rect.setAttribute("class", "box" + (state.selected === id ? " sel" : ""));
    rect.setAttribute("width", String(c.width));
    rect.setAttribute("height", String(c.height));
    rect.setAttribute("rx", "5");
    const node = graph.nodes.find((n) => n.id === id);
    rect.onclick = () => selectNode(id);
    rect.ondblclick = () => {
      if (node?.expandable) setScope(node.path ?? "", node.label);
    };
    g.appendChild(rect);

    const label = document.createElementNS(SVGNS, "text");
    label.setAttribute("class", "box-label");
    label.setAttribute("x", String(c.width / 2));
    label.setAttribute("y", "16");
    label.setAttribute("text-anchor", "middle");
    label.textContent = c.labels?.[0]?.text + (node?.expandable ? " ▸" : "");
    g.appendChild(label);

    for (const p of c.ports ?? []) {
      const dot = document.createElementNS(SVGNS, "circle");
      dot.setAttribute("class", "port");
      dot.setAttribute("cx", String(p.x ?? 0));
      dot.setAttribute("cy", String(p.y ?? 0));
      dot.setAttribute("r", "3");
      g.appendChild(dot);
    }
    svg.appendChild(g);
  }
  host.appendChild(svg);
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

function init() {
  ($("model") as HTMLInputElement).value =
    "../../fixtures/picorv32_soc/golden/hierarchy.json";
  ($("trace") as HTMLInputElement).value =
    "../../fixtures/picorv32_soc/traces/picorv32_soc.fst";
  ($("srcroot") as HTMLInputElement).value = "../..";
  $("load").addEventListener("click", load);
}

document.addEventListener("DOMContentLoaded", init);

// Keep nodeId referenced for potential external callers / tree-shaking clarity.
export { nodeId };
