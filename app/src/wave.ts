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

// Draw one trace into its own canvas on the shared time axis [0, tMax].
export function drawTrack(
  canvas: HTMLCanvasElement,
  values: ValueChange[],
  tMax: number,
): void {
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const w = canvas.width;
  const h = canvas.height;
  ctx.clearRect(0, 0, w, h);
  if (!values.length) return;
  const geom = { hi: 5, lo: h - 5, mid: h / 2, w };
  const xOf = (t: number) => (tMax > 0 ? (t / tMax) * (w - 1) : 0);
  const segs = buildSegments(values, tMax);
  if (isDigital(values)) drawDigital(ctx, segs, xOf, geom);
  else drawBus(ctx, segs, xOf, geom);
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
    const label = seg.value.length <= 10 ? seg.value : `${seg.value.slice(0, 9)}…`;
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
