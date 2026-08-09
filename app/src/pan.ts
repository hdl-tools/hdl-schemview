// Pure logic for drag-to-pan in the schematic pane (#265). DOM-free so the
// gesture's decisions are unit-testable (like schempick.ts / prefs.ts); main.ts
// owns the listeners, the class toggles and the scroll writes.
import { isTextEntryTag } from "./schempick";

// Which press starts a pan: the middle button always, the left button only while
// Space is held. Plain left-drag is deliberately *not* a pan — it stays with the
// pane's own select and drill handlers.
export function shouldStartPan(button: number, spaceHeld: boolean): boolean {
  return button === 1 || (button === 0 && spaceHeld);
}

// Scroll offsets for a drag of (dx, dy) measured *from the gesture's start*,
// clamped to the host's scrollable range.
//
// Deltas are taken against the captured start rather than the previous move, so a
// pan that runs past an edge and comes back lands where the cursor says instead of
// drifting by the overshoot the clamp swallowed.
//
// The browser clamps a `scrollLeft` write too, so the clamp here is belt-and-braces
// — but taking `max` as a parameter is the point: it lets the caller read
// scrollWidth/clientWidth once per gesture instead of once per move, which is what
// keeps the drag from forcing a layout flush on every frame. A `max` below zero
// (content narrower than the viewport) degenerates to 0 rather than inverting.
export function panTarget(
  start: { left: number; top: number },
  dx: number,
  dy: number,
  max: { left: number; top: number },
): { left: number; top: number } {
  const clamp = (v: number, hi: number) => Math.max(0, Math.min(v, Math.max(0, hi)));
  return { left: clamp(start.left - dx, max.left), top: clamp(start.top - dy, max.top) };
}

// Whether keyboard focus belongs to something that owns the Space key itself, so
// holding Space there doesn't arm the pan. Text entry is the same guard the palette's
// `a` hotkey uses (#219) — reused rather than restated, so the two hotkeys cannot
// drift apart. The rest are the elements browsers activate on Space: arming the pan
// calls preventDefault, which would otherwise swallow a focused toolbar button's own
// activation.
export function blocksSpaceHotkey(tagName: string, isContentEditable: boolean): boolean {
  if (isTextEntryTag(tagName, isContentEditable)) return true;
  return tagName === "BUTTON" || tagName === "A" || tagName === "SUMMARY";
}
