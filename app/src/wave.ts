// Multi-signal waveform rendering (#15). Each signal occupies its own row: a name
// cell, a per-row <canvas> track, and reorder/remove controls (laid out by main.ts).
//
// Pure, DOM-free helpers (time axis, signal-type detection, segment building) live
// here so they are unit-testable the way elk.ts is; the canvas drawing functions
// (`drawTrack` and its private helpers) are the only DOM-touching code.

import type { ValueChange } from "./types";

// A signal pinned to the waveform pane: its trace `ref` (the wellen signal_ref used
// to fetch values), display `name`, and the fetched value-change samples.
export interface WaveTrace {
  ref: number;
  name: string;
  values: ValueChange[];
}

// Fixed per-track canvas height (px). Must match `.wave-track` height in style.css.
export const TRACK_H = 28;

// NOTE: assumes samples are time-ordered (VCD/FST are); only the last sample's time
// is read, so an unordered array would mis-scale the axis.
export function maxTime(traces: WaveTrace[]): number {
  let t = 1;
  for (const tr of traces) {
    const last = tr.values[tr.values.length - 1];
    if (last && last.time > t) t = last.time;
  }
  return t;
}

// A trace is digital when every sample is a single character ("0"/"1"/"x"/"z");
// multi-character samples are buses, drawn as value-annotated hexagon segments.
export function isDigital(values: ValueChange[]): boolean {
  return values.every((v) => v.value.length <= 1);
}

// A single-bit sample that is neither driven-0 nor driven-1 (x/z/u/-): drawn as a
// shaded band rather than a high or low rail.
export function isUnknown(value: string): boolean {
  return value !== "0" && value !== "1";
}

export interface Segment {
  t0: number;
  t1: number;
  value: string;
}

// Turn value-change samples into held segments: each value persists until the next
// change, and the final value holds to `tMax` (so short traces extend to the shared
// end of the window).
export function buildSegments(values: ValueChange[], tMax: number): Segment[] {
  const segs: Segment[] = [];
  for (let i = 0; i < values.length; i++) {
    const t0 = values[i].time;
    const t1 = i + 1 < values.length ? values[i + 1].time : Math.max(tMax, t0);
    segs.push({ t0, t1, value: values[i].value });
  }
  return segs;
}

// The visible time window, shared by every track and the ruler.
export interface TimeWindow {
  t0: number;
  t1: number;
}

// Primary (A, left-click) and secondary (B, right-click) marker times; null = unset.
export interface Markers {
  a: number | null;
  b: number | null;
}

export interface Tick {
  time: number;
  x: number;
  label: string;
}

// Map a time within [t0, t1] to a pixel x in [0, w] (and back). Degenerate-safe.
export function timeToX(t: number, t0: number, t1: number, w: number): number {
  return t1 > t0 ? ((t - t0) / (t1 - t0)) * w : 0;
}
export function xToTime(x: number, t0: number, t1: number, w: number): number {
  return w > 0 ? t0 + (x / w) * (t1 - t0) : t0;
}

// Zoom the window about `pivotT` (the time under the cursor): factor<1 zooms in,
// >1 zooms out. The pivot keeps its relative position; result clamps to [0, max]
// with a minimum span so zoom can't collapse.
export function zoomWindow(
  view: TimeWindow,
  factor: number,
  pivotT: number,
  max: number,
): TimeWindow {
  const span = view.t1 - view.t0;
  const minSpan = max > 0 ? max / 100000 : 1;
  const newSpan = Math.min(max > 0 ? max : span, Math.max(minSpan, span * factor));
  const frac = span > 0 ? (pivotT - view.t0) / span : 0.5;
  let t0 = pivotT - frac * newSpan;
  let t1 = t0 + newSpan;
  if (t0 < 0) {
    t1 -= t0;
    t0 = 0;
  }
  if (t1 > max) {
    t0 -= t1 - max;
    t1 = max;
  }
  if (t0 < 0) t0 = 0;
  return { t0, t1 };
}

// Shift the window by `dt`, preserving its span and clamping to [0, max].
export function panWindow(view: TimeWindow, dt: number, max: number): TimeWindow {
  const span = view.t1 - view.t0;
  let t0 = view.t0 + dt;
  let t1 = t0 + span;
  if (t0 < 0) {
    t0 = 0;
    t1 = span;
  }
  if (t1 > max) {
    t1 = max;
    t0 = max - span;
  }
  if (t0 < 0) t0 = 0;
  return { t0, t1 };
}

// The value held at time `t` (the last change with time <= t); "" before the first
// sample. Drives the per-row value-at-marker column. Assumes time-ordered samples.
export function valueAt(values: ValueChange[], t: number): string {
  let v = "";
  for (const c of values) {
    if (c.time <= t) v = c.value;
    else break;
  }
  return v;
}

// Display units offered in the unit picker, smallest → largest.
export const DISPLAY_UNITS = ["ps", "ns", "us", "ms"] as const;
export type DisplayUnit = (typeof DISPLAY_UNITS)[number];

// Power-of-ten seconds exponent for a normalized short unit string.
const UNIT_EXP: Record<string, number> = {
  zs: -21,
  as: -18,
  fs: -15,
  ps: -12,
  ns: -9,
  us: -6,
  ms: -3,
  s: 0,
};

export function unitExponent(unit: string): number | null {
  return unit in UNIT_EXP ? UNIT_EXP[unit] : null;
}

// Display units per raw tick: convert ticks → native seconds → display unit. Returns
// 1 (identity) when the native timescale or either unit is unknown.
export function displayScale(
  timescale: { factor: number; unit: string } | null,
  unit: string,
): number {
  if (!timescale) return 1;
  const nExp = unitExponent(timescale.unit);
  const dExp = unitExponent(unit);
  if (nExp == null || dExp == null) return 1;
  return timescale.factor * Math.pow(10, nExp - dExp);
}

// Pick a sensible initial display unit: the native unit when it is selectable, else ns.
export function defaultDisplayUnit(
  timescale: { factor: number; unit: string } | null,
): DisplayUnit {
  if (timescale && (DISPLAY_UNITS as readonly string[]).includes(timescale.unit)) {
    return timescale.unit as DisplayUnit;
  }
  return "ns";
}

// Drop redundant leading zeros from a bus value for display ("00000018" → "18",
// "00000000" → "0"); empty stays empty. Pure.
export function trimBusValue(value: string): string {
  if (!value) return value;
  const trimmed = value.replace(/^0+/, "");
  return trimmed === "" ? "0" : trimmed;
}

// The value-change time closest to `t` (markers snap to edges); null if no samples.
export function nearestEdge(values: ValueChange[], t: number): number | null {
  if (!values.length) return null;
  let best = values[0].time;
  let bestD = Math.abs(best - t);
  for (const c of values) {
    const d = Math.abs(c.time - t);
    if (d < bestD) {
      bestD = d;
      best = c.time;
    }
  }
  return best;
}

// "Nice" 1/2/5×10ⁿ tick step (~one tick per 80px). Pure.
function niceStep(raw: number): number {
  if (raw <= 0) return 1;
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const norm = raw / mag;
  const f = norm < 1.5 ? 1 : norm < 3 ? 2 : norm < 7 ? 5 : 10;
  return f * mag;
}

// Evenly spaced ruler ticks for the visible window, each with its pixel x and label.
export function tickMarks(t0: number, t1: number, w: number): Tick[] {
  if (t1 <= t0 || w <= 0) return [];
  const step = niceStep((t1 - t0) / Math.max(1, Math.floor(w / 80)));
  const decimals = Math.max(0, -Math.floor(Math.log10(step)));
  const ticks: Tick[] = [];
  for (let k = Math.ceil(t0 / step); k * step <= t1; k++) {
    const time = k * step;
    ticks.push({ time, x: timeToX(time, t0, t1, w), label: time.toFixed(decimals) });
  }
  return ticks;
}

// Draw one trace into its own canvas, scaled/clipped to the visible window, with the
// A/B marker lines overlaid.
export function drawTrack(
  canvas: HTMLCanvasElement,
  values: ValueChange[],
  view: TimeWindow,
  markers: Markers,
): void {
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const w = canvas.width;
  const h = canvas.height;
  ctx.clearRect(0, 0, w, h);
  const xOf = (t: number) => timeToX(t, view.t0, view.t1, w);
  if (values.length) {
    const geom = { hi: 5, lo: h - 5, mid: h / 2, w };
    const segs = buildSegments(values, view.t1);
    if (isDigital(values)) drawDigital(ctx, segs, xOf, geom);
    else drawBus(ctx, segs, xOf, geom);
  }
  drawMarkerLines(ctx, markers, xOf, w, h);
}

// Vertical A/B marker lines, drawn only when inside the canvas.
function drawMarkerLines(
  ctx: CanvasRenderingContext2D,
  markers: Markers,
  xOf: (t: number) => number,
  w: number,
  h: number,
): void {
  const draw = (t: number | null, color: string) => {
    if (t == null) return;
    const x = xOf(t);
    if (x < 0 || x > w) return;
    ctx.strokeStyle = color;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(x + 0.5, 0);
    ctx.lineTo(x + 0.5, h);
    ctx.stroke();
  };
  draw(markers.a, "#ffd24d"); // primary A
  draw(markers.b, "#4dd2ff"); // secondary B
}

// Draw the time-axis ruler: tick marks + labels and the A/B marker positions.
export function drawRuler(
  canvas: HTMLCanvasElement,
  view: TimeWindow,
  markers: Markers,
  scale = 1,
): void {
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const w = canvas.width;
  const h = canvas.height;
  ctx.clearRect(0, 0, w, h);
  ctx.strokeStyle = "rgba(128,128,128,0.5)";
  ctx.fillStyle = "#888";
  ctx.lineWidth = 1;
  ctx.textBaseline = "bottom";
  ctx.beginPath();
  ctx.moveTo(0, h - 0.5);
  ctx.lineTo(w, h - 0.5);
  ctx.stroke();
  // Tick values/labels are computed in display units (window × scale); the pixel x
  // is scale-invariant, so positions still line up with the raw-time tracks.
  for (const t of tickMarks(view.t0 * scale, view.t1 * scale, w)) {
    ctx.beginPath();
    ctx.moveTo(t.x + 0.5, h);
    ctx.lineTo(t.x + 0.5, h - 4);
    ctx.stroke();
    ctx.fillText(t.label, t.x + 2, h - 4);
  }
  const tick = (mt: number | null, color: string) => {
    if (mt == null) return;
    const x = timeToX(mt, view.t0, view.t1, w);
    if (x < 0 || x > w) return;
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.moveTo(x, h);
    ctx.lineTo(x - 3, h - 5);
    ctx.lineTo(x + 3, h - 5);
    ctx.closePath();
    ctx.fill();
  };
  tick(markers.a, "#ffd24d");
  tick(markers.b, "#4dd2ff");
}

interface Geom {
  hi: number;
  lo: number;
  mid: number;
  w: number;
}

// Single-bit: low/high rails joined by vertical edges; x/z drawn as a shaded band.
function drawDigital(
  ctx: CanvasRenderingContext2D,
  segs: Segment[],
  xOf: (t: number) => number,
  g: Geom,
): void {
  ctx.lineWidth = 1.5;
  let prevY: number | null = null;
  for (const seg of segs) {
    const x0 = xOf(seg.t0);
    const x1 = xOf(seg.t1);
    if (isUnknown(seg.value)) {
      ctx.fillStyle = "rgba(229,115,115,0.30)";
      ctx.fillRect(x0, g.hi, Math.max(1, x1 - x0), g.lo - g.hi);
      ctx.strokeStyle = "#e57373";
      ctx.beginPath();
      ctx.moveTo(x0, g.hi);
      ctx.lineTo(x1, g.hi);
      ctx.moveTo(x0, g.lo);
      ctx.lineTo(x1, g.lo);
      ctx.stroke();
      prevY = null; // an unknown band breaks rail continuity
      continue;
    }
    const y = seg.value === "1" ? g.hi : g.lo;
    ctx.strokeStyle = "#7fd";
    if (prevY != null && prevY !== y) {
      ctx.beginPath();
      ctx.moveTo(x0, prevY);
      ctx.lineTo(x0, y);
      ctx.stroke();
    }
    ctx.beginPath();
    ctx.moveTo(x0, y);
    ctx.lineTo(x1, y);
    ctx.stroke();
    prevY = y;
  }
}

// Multi-bit bus: parallel top/bottom rails that cross over at each transition
// (the hexagon look), with the value centred in each segment when it fits.
function drawBus(
  ctx: CanvasRenderingContext2D,
  segs: Segment[],
  xOf: (t: number) => number,
  g: Geom,
): void {
  const s = 3; // half-width of the transition crossover
  ctx.lineWidth = 1.25;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  segs.forEach((seg, i) => {
    const x0 = xOf(seg.t0);
    const x1 = i === segs.length - 1 ? g.w : xOf(seg.t1);
    const a = i === 0 ? x0 : x0 + s;
    const b = i === segs.length - 1 ? x1 : x1 - s;
    ctx.strokeStyle = "#9cf";
    if (b > a) {
      ctx.beginPath();
      ctx.moveTo(a, g.hi);
      ctx.lineTo(b, g.hi);
      ctx.moveTo(a, g.lo);
      ctx.lineTo(b, g.lo);
      ctx.stroke();
    }
    if (i === 0) {
      // Open the first segment with a left-facing point.
      ctx.beginPath();
      ctx.moveTo(x0, g.mid);
      ctx.lineTo(a, g.hi);
      ctx.moveTo(x0, g.mid);
      ctx.lineTo(a, g.lo);
      ctx.stroke();
    }
    const v = trimBusValue(seg.value);
    const label = v.length <= 10 ? v : `${v.slice(0, 9)}…`;
    ctx.fillStyle = "#cfe0ff";
    if (b - a > ctx.measureText(label).width + 4) ctx.fillText(label, (a + b) / 2, g.mid);
  });
  // Crossover diagonals at each internal transition.
  ctx.strokeStyle = "#9cf";
  for (let i = 1; i < segs.length; i++) {
    const x = xOf(segs[i].t0);
    ctx.beginPath();
    ctx.moveTo(x - s, g.hi);
    ctx.lineTo(x + s, g.lo);
    ctx.moveTo(x - s, g.lo);
    ctx.lineTo(x + s, g.hi);
    ctx.stroke();
  }
}
