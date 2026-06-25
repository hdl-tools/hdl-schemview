// hdl-schemview frontend: three panes (schematic / source / waveform) linked by
// one selection, resolved through the cross-probe commands.
import { api } from "./api";
import { layout, nodeId } from "./elk";
import type { ProbeResponse, SchematicGraph, SchPort, ValueChange } from "./types";

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

  // Wires first (under everything), then their net labels, then the boxes.
  for (const e of laid.edges ?? []) {
    for (const sec of e.sections ?? []) {
      const pts = [sec.startPoint, ...(sec.bendPoints ?? []), sec.endPoint];
      const path = document.createElementNS(SVGNS, "polyline");
      path.setAttribute("class", "wire");
      path.setAttribute("points", pts.map((p: any) => `${p.x},${p.y}`).join(" "));
      svg.appendChild(path);
    }
    for (const lab of e.labels ?? []) {
      if (!lab.text) continue;
      const t = document.createElementNS(SVGNS, "text");
      t.setAttribute("class", "wire-label");
      t.setAttribute("x", String((lab.x ?? 0) + (lab.width ?? 0) / 2));
      t.setAttribute("y", String((lab.y ?? 0) + (lab.height ?? 11) - 1));
      t.setAttribute("text-anchor", "middle");
      t.textContent = lab.text;
      svg.appendChild(t);
    }
  }

  for (const c of laid.children ?? []) {
    const id = Number(String(c.id).slice(1));
    const node = graph.nodes.find((n) => n.id === id);
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

    // Pins: a direction arrow at each port (in on the west, out on the east)
    // plus the port name + bit-width, drawn just inside the border.
    for (const p of c.ports ?? []) {
      const pid = Number(String(p.id).slice(1));
      const sp = portById.get(pid);
      const px = p.x ?? 0;
      const py = p.y ?? 0;
      const west = sp ? sp.side !== "east" : px < c.width / 2;

      const arrow = document.createElementNS(SVGNS, "path");
      arrow.setAttribute("class", "pin " + (west ? "pin-in" : "pin-out"));
      arrow.setAttribute(
        "d",
        west
          ? `M${px - 9},${py - 4} L${px - 9},${py + 4} L${px},${py} Z`
          : `M${px},${py - 4} L${px},${py + 4} L${px + 9},${py} Z`,
      );
      arrow.onclick = () => selectNode(pid);
      g.appendChild(arrow);

      if (sp) {
        const t = document.createElementNS(SVGNS, "text");
        t.setAttribute("class", "pin-label");
        t.setAttribute("x", String(west ? px + 6 : px - 6));
        t.setAttribute("y", String(py + 3));
        t.setAttribute("text-anchor", west ? "start" : "end");
        t.textContent = sp.width ? `${sp.name}${sp.width}` : sp.name;
        t.onclick = () => selectNode(pid);
        g.appendChild(t);
      }
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
}

document.addEventListener("DOMContentLoaded", init);

// Keep nodeId referenced for potential external callers / tree-shaking clarity.
export { nodeId };
