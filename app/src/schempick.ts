// Pure logic for the schematic pane's signal-tracing palette (#219) and for trace
// mode's step list (#244 PR3). DOM-free so it is unit-testable (like prefs.ts /
// log.ts); main.ts owns the overlay and canvas wiring.
import type { Dir, SignalEntry, TraceStep } from "./types";

// Case-insensitive substring filter over signal names — the palette's search box.
// A blank/whitespace query returns the full list unchanged.
export function filterSignals(sigs: SignalEntry[], query: string): SignalEntry[] {
  const q = query.trim().toLowerCase();
  if (!q) return sigs;
  return sigs.filter((s) => s.name.toLowerCase().includes(q));
}

// Move the palette's keyboard highlight by `delta`, clamped to [0, len-1] (no wrap).
// An empty list clamps to 0 rather than yielding a negative index.
export function moveIndex(current: number, delta: number, len: number): number {
  if (len <= 0) return 0;
  return Math.max(0, Math.min(current + delta, len - 1));
}

// Whether keyboard focus is in a text-entry element, so the palette's bare `a`
// hotkey (no modifier) doesn't fire while the user is typing (e.g. the load form,
// the palette's own search box, or a contentEditable region).
export function isTextEntryTag(tagName: string, isContentEditable: boolean): boolean {
  if (isContentEditable) return true;
  return tagName === "INPUT" || tagName === "TEXTAREA" || tagName === "SELECT";
}

// ---------------------------------------------------------------------------
// Trace mode's step list (#244 PR3)
// ---------------------------------------------------------------------------

// Append an expansion step, unless the list already holds an identical one.
//
// The backend re-derives the whole trace from this list on every change, so a
// repeated `(path, dir)` would cost a re-walk and change nothing — re-clicking
// the same affordance is idempotent rather than a slow no-op. Steps are otherwise
// kept in order, including a second step on the same signal in the *other*
// direction: that is how a net's fan-in and fan-out meet at one junction.
//
// Returns a new array; never mutates the input.
export function pushTraceStep(steps: TraceStep[], step: TraceStep): TraceStep[] {
  const same = (a: TraceStep, b: TraceStep) =>
    a.path === b.path && a.dir === b.dir && (a.depth ?? 1) === (b.depth ?? 1);
  if (steps.some((s) => same(s, step))) return steps;
  return [...steps, step];
}

// Drop every step after index `i`, keeping `i` itself — the trace bar's "rewind to
// here", mirroring how a breadcrumb frame truncates the scope stack. An index
// outside the list leaves it unchanged, so a stale click cannot empty the canvas.
export function truncateTrace(steps: TraceStep[], i: number): TraceStep[] {
  if (i < 0 || i >= steps.length) return steps;
  return steps.slice(0, i + 1);
}

// A step's label for the trace bar: the signal's own name (the last path segment,
// which is what the schematic labels a wire with) plus the direction it was
// expanded in. Bit-selects and indices survive because only the final `.` splits.
export function stepLabel(step: TraceStep): string {
  const name = step.path.split(".").pop() || step.path;
  return `${name} ${dirGlyph(step.dir)}`;
}

// Fan-in points back at what drives the signal, fan-out forward at what reads it;
// `inout` is both. Kept here rather than in the DOM code so the label and any test
// of it share one source.
export function dirGlyph(dir: Dir): string {
  return dir === "in" ? "◀" : dir === "out" ? "▶" : "◀▶";
}
