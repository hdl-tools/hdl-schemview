// Pure logic for the schematic pane's signal-tracing palette (#219): the `a`-key
// pop-up that lists the signals in the schematic's current scope. DOM-free so it is
// unit-testable (like prefs.ts / log.ts); main.ts owns the overlay wiring.
import type { SignalEntry } from "./types";

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
