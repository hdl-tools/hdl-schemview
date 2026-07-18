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
  // Stable per-lane identity (#179). `ref` no longer identifies a lane: a signal can be
  // pinned as several lanes (each with its own radix), so they share a `ref`; a sub-bus
  // uses a synthetic negative `ref`. `key` is unique per lane and survives reorder and
  // the snapshot round-trip, so grouping/addressing has something to hold onto.
  key: number;
  ref: number;
  name: string;
  path?: string; // canonical model node path, so a lane re-resolves across traces (#170)
  // A derived sub-bus of the signal at `path` (#179): `parent[hi:lo]`. Carried so a
  // trace swap re-derives the slice from the parent's new values instead of dropping
  // the lane. Absent on a plain lane.
  slice?: { hi: number; lo: number };
  values: ValueChange[];
  radix?: Radix; // per-signal display radix; defaults to hex for multi-bit buses
  enumMap?: Map<number, string>; // value→name for enum/FSM signals (#81)
  showName?: boolean; // when an enumMap is present, show the state name (default true)
}

// Re-resolve a lane against a freshly-loaded trace (#179, the "Load trace…" swap). A
// plain lane adopts the new `ref` and the trace's values; a sub-bus keeps its synthetic
// `ref` and re-slices the parent's new `values` so it stays `parent[hi:lo]` rather than
// silently reverting to the full word (or being dropped). Pure: the caller fetches
// `fullValues` for `tr.path` from the new trace and hands them in.
export function reresolveLane(
  tr: WaveTrace,
  ref: number,
  fullValues: ValueChange[],
): WaveTrace {
  if (tr.slice) {
    const { hi, lo } = tr.slice;
    const values = fullValues.map((c) => ({ time: c.time, value: sliceBits(c.value, hi, lo) }));
    return { ...tr, values };
  }
  return { ...tr, ref, values: fullValues };
}

// The next lane-key and synthetic-ref seeds for a pane reseeded from a snapshot (#179).
// Keys count up from 1, sub-bus refs down from -1; a restored lane may already hold
// either, so the counters must resume past every existing key and below every existing
// negative ref, or a pop-out would re-mint a collision. Positive refs (real signals)
// don't constrain the synthetic-ref counter.
export function laneCounterSeeds(
  waves: readonly { key: number; ref: number }[],
): { laneKey: number; derivedRef: number } {
  let laneKey = 1;
  let derivedRef = -1;
  for (const w of waves) {
    if (w.key >= laneKey) laneKey = w.key + 1;
    if (w.ref <= derivedRef) derivedRef = w.ref - 1;
  }
  return { laneKey, derivedRef };
}

// A named, collapsible group of lanes (#182). The waveform pane is organized entirely
// into groups — there are no loose lanes — and always keeps one empty group at the
// bottom as the landing spot for a new group (see `withTrailingEmptyGroup`).
export interface WaveGroup {
  name: string;
  collapsed: boolean;
  waves: WaveTrace[];
}

// Every lane across every group, in order — the flat view the index-based lane code and
// `maxTime`/dedupe still work against.
export function flattenLanes(groups: readonly WaveGroup[]): WaveTrace[] {
  return groups.flatMap((g) => g.waves);
}

// The lanes actually drawn: those of non-collapsed groups, in order. A collapsed group
// contributes its header row but none of its tracks, so `redrawTracks` maps canvases
// against this, not `flattenLanes`.
export function visibleLanes(groups: readonly WaveGroup[]): WaveTrace[] {
  return groups.filter((g) => !g.collapsed).flatMap((g) => g.waves);
}

// Enforce half of the pane invariant (#182): the last group is always empty, a drop
// target for starting a new group. Returns the input unchanged when it already holds
// (last group empty), a fresh single empty group for an empty pane, else the input with
// one empty group appended. Identity is preserved on the no-op so callers can `===`-check.
export function withTrailingEmptyGroup(
  groups: WaveGroup[],
  makeName: () => string,
): WaveGroup[] {
  const last = groups[groups.length - 1];
  if (last && last.waves.length === 0) return groups;
  return [...groups, { name: makeName(), collapsed: false, waves: [] }];
}

// The pane invariant (#182, amended #188): every user-created group persists — a group
// emptied by a remove/move is *preserved*, not pruned, since groups are user-authored
// containers — and the pane always ends with an empty group as the landing spot for
// starting a new one. A fresh or all-empty pane becomes a single empty group. Identity is
// preserved (returns the input) when the last group is already empty, so an auto-name
// doesn't churn on every mutation. Run after any mutation of the groups.
export function normalizeGroups(groups: WaveGroup[], makeName: () => string): WaveGroup[] {
  return withTrailingEmptyGroup(groups, makeName);
}

// Where a newly added signal lands (#182): the last populated group, or the last group
// when the pane is all-empty (the default group of a fresh pane). New signals accumulate
// in the working group; the trailing empty group is reserved for starting a new group.
export function workingGroupIndex(groups: readonly WaveGroup[]): number {
  for (let i = groups.length - 1; i >= 0; i--) {
    if (groups[i].waves.length > 0) return i;
  }
  return groups.length - 1;
}

// Move the lane with `key` to a new slot (#188 drag-reorder): a destination group index
// and an insertion index within that group's waves, both in the *current* array's
// coordinates (the slot the drop indicator sits before; `waves.length` for the end).
// Returns a new groups array — the source and destination groups get fresh waves arrays,
// other groups keep their identity — so callers can reassign without mutating state. When
// source and destination are the same group and the lane moves forward, the index is
// adjusted for the gap left by removing it, so a drop "just after itself" is a no-op.
// Dropping a lane onto its own position, an unknown key, or an out-of-range group all
// return the input unchanged. Callers should `normalizeGroups` the result to re-establish
// the trailing-empty-group invariant (a drop into the empty group spawns a fresh one).
export function moveLaneTo(
  groups: WaveGroup[],
  key: number,
  destGroup: number,
  destIndex: number,
): WaveGroup[] {
  if (destGroup < 0 || destGroup >= groups.length) return groups;
  let sg = -1;
  let si = -1;
  for (let g = 0; g < groups.length; g++) {
    const i = groups[g].waves.findIndex((w) => w.key === key);
    if (i >= 0) {
      sg = g;
      si = i;
      break;
    }
  }
  if (sg < 0) return groups;
  // Dropping onto its own slot (before or just after itself) changes nothing.
  if (sg === destGroup && (si === destIndex || si === destIndex - 1)) return groups;
  const next = groups.map((g) => ({ ...g, waves: g.waves.slice() }));
  const [lane] = next[sg].waves.splice(si, 1);
  let di = destIndex;
  if (sg === destGroup && si < destIndex) di -= 1;
  di = Math.max(0, Math.min(di, next[destGroup].waves.length));
  next[destGroup].waves.splice(di, 0, lane);
  return next;
}

// Fixed per-track canvas height (px). Must match `.wave-track` height in style.css.
export const TRACK_H = 20;

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

// Per-signal value radix. Native trace values are binary strings (MSB→LSB).
export type Radix = "bin" | "oct" | "dec" | "hex";

// Group a binary string into `bits`-wide digits from the LSB; a group with any
// non-0/1 bit renders as "x". Used for hex (4) / octal (3).
function groupRadix(binary: string, bits: number, digits: string): string {
  const pad = (bits - (binary.length % bits)) % bits;
  const s = "0".repeat(pad) + binary;
  let out = "";
  for (let i = 0; i < s.length; i += bits) {
    const group = s.slice(i, i + bits);
    out += /[^01]/.test(group) ? "x" : digits[parseInt(group, 2)];
  }
  return out;
}

// Convert a native binary value string to `radix`, with leading zeros trimmed. Any
// unknown bit (x/z/…) makes its hex/oct digit — or the whole decimal — "x". Pure.
export function formatValue(binary: string, radix: Radix): string {
  if (!binary) return "";
  switch (radix) {
    case "bin":
      return trimBusValue(binary);
    case "hex":
      return trimBusValue(groupRadix(binary, 4, "0123456789abcdef"));
    case "oct":
      return trimBusValue(groupRadix(binary, 3, "01234567"));
    case "dec":
      return /[^01]/.test(binary) ? "x" : BigInt(`0b${binary}`).toString(10);
  }
}

// Decode a binary value to its enum state name, or null when the value has unknown
// bits (x/z) or isn't a mapped encoding (caller falls back to the numeric radix).
export function enumName(binary: string, map: Map<number, string>): string | null {
  if (!binary || /[^01]/.test(binary)) return null;
  return map.get(parseInt(binary, 2)) ?? null;
}

// The display string for a value: the enum state name when in name mode and it
// resolves, otherwise the value formatted in `radix`.
export function displayValue(
  binary: string,
  radix: Radix,
  enumMap?: Map<number, string>,
  showName?: boolean,
): string {
  if (showName && enumMap) {
    const name = enumName(binary, enumMap);
    if (name != null) return name;
  }
  return formatValue(binary, radix);
}

// X for a bus segment's value label: centred in the segment's on-screen portion
// (its intersection with [0, w]), so a wide segment's value stays in view and re-
// centres as you pan/zoom. null when the visible portion can't fit a label of `labelW`.
export function visibleLabelX(a: number, b: number, w: number, labelW: number): number | null {
  const va = Math.max(a, 1);
  const vb = Math.min(b, w - 1);
  if (vb - va < labelW + 4) return null;
  return (va + vb) / 2;
}

// Extract bits [hi:lo] (Verilog order) from a binary string. `bit_string` is
// MSB→LSB, so bit b sits at char w-1-b. Bounds clamp to [0, w-1]; hi < lo → "". Pure.
export function sliceBits(binary: string, hi: number, lo: number): string {
  const w = binary.length;
  const h = Math.min(hi, w - 1);
  const l = Math.max(lo, 0);
  if (h < l) return "";
  return binary.slice(w - 1 - h, w - l);
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

// The value to show at marker time `t`, trimmed for display. When `t` lands exactly
// on a transition edge, show `prev -> next` (collapsed to one when unchanged); off an
// edge (or at the first sample) show the single held value.
export function valueAtMarker(
  values: ValueChange[],
  t: number,
  radix: Radix = "bin",
  enumMap?: Map<number, string>,
  showName?: boolean,
): string {
  const fmt = (v: string) => displayValue(v, radix, enumMap, showName);
  const i = values.findIndex((c) => c.time === t);
  if (i > 0) {
    const prev = fmt(values[i - 1].value);
    const cur = fmt(values[i].value);
    return prev === cur ? cur : `${prev} -> ${cur}`;
  }
  if (i === 0) return fmt(values[0].value);
  return fmt(valueAt(values, t));
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
  radix: Radix = "hex",
  enumMap?: Map<number, string>,
  showName?: boolean,
): void {
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const w = canvas.width;
  const h = canvas.height;
  ctx.clearRect(0, 0, w, h);
  const xOf = (t: number) => timeToX(t, view.t0, view.t1, w);
  if (values.length) {
    const geom = { hi: 3, lo: h - 3, mid: h / 2, w };
    const segs = buildSegments(values, view.t1);
    if (isDigital(values)) drawDigital(ctx, segs, xOf, geom);
    else drawBus(ctx, segs, xOf, geom, radix, enumMap, showName);
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
  radix: Radix,
  enumMap?: Map<number, string>,
  showName?: boolean,
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
    const v = displayValue(seg.value, radix, enumMap, showName);
    const label = v.length <= 10 ? v : `${v.slice(0, 9)}…`;
    ctx.fillStyle = "#cfe0ff";
    const labelX = visibleLabelX(a, b, g.w, ctx.measureText(label).width);
    if (labelX != null) ctx.fillText(label, labelX, g.mid);
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
