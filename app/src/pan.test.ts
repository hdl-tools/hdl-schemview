import { describe, it, expect } from "vitest";
import { blocksSpaceHotkey, panTarget, shouldStartPan } from "./pan";

describe("shouldStartPan", () => {
  it("starts on the middle button with no modifier held", () => {
    expect(shouldStartPan(1, false)).toBe(true);
  });

  it("starts on the left button only while Space is held", () => {
    expect(shouldStartPan(0, true)).toBe(true);
    expect(shouldStartPan(0, false)).toBe(false);
  });

  it("never starts on the right or the back/forward buttons", () => {
    for (const button of [2, 3, 4]) {
      expect(shouldStartPan(button, false)).toBe(false);
      expect(shouldStartPan(button, true)).toBe(false);
    }
  });

  it("still starts on middle while Space is held", () => {
    // The modifier arms left-drag; it must not disqualify the button that already
    // pans on its own.
    expect(shouldStartPan(1, true)).toBe(true);
  });
});

describe("panTarget", () => {
  const start = { left: 100, top: 100 };
  const max = { left: 500, top: 400 };

  it("moves the content with the cursor", () => {
    // Dragging right (+dx) pulls the content right, which means scrolling left.
    expect(panTarget(start, 30, 20, max)).toEqual({ left: 70, top: 80 });
    expect(panTarget(start, -30, -20, max)).toEqual({ left: 130, top: 120 });
  });

  it("leaves the offsets untouched for a zero delta", () => {
    expect(panTarget(start, 0, 0, max)).toEqual({ left: 100, top: 100 });
  });

  it("measures every delta from the gesture start, so an overshoot cannot drift", () => {
    // Run far past the left edge (clamped to 0), then come back to where the drag
    // began. Because the delta is taken against the captured start rather than the
    // previous position, the swallowed overshoot is not carried into the return.
    expect(panTarget(start, 400, 0, max)).toEqual({ left: 0, top: 100 });
    expect(panTarget(start, 0, 0, max)).toEqual({ left: 100, top: 100 });
  });

  it("clamps at the top-left rather than producing a negative offset", () => {
    expect(panTarget(start, 999, 999, max)).toEqual({ left: 0, top: 0 });
  });

  it("clamps at the bottom-right end of the scrollable range", () => {
    expect(panTarget(start, -999, -999, max)).toEqual({ left: 500, top: 400 });
  });

  it("pins an axis whose content fits the viewport to 0", () => {
    expect(panTarget(start, -50, -50, { left: 0, top: 0 })).toEqual({ left: 0, top: 0 });
  });

  it("treats a negative max as 0 rather than inverting the range", () => {
    expect(panTarget(start, -50, -50, { left: -20, top: -20 })).toEqual({
      left: 0,
      top: 0,
    });
  });
});

describe("blocksSpaceHotkey", () => {
  it("blocks the same text-entry targets the `a` hotkey does", () => {
    expect(blocksSpaceHotkey("INPUT", false)).toBe(true);
    expect(blocksSpaceHotkey("TEXTAREA", false)).toBe(true);
    expect(blocksSpaceHotkey("SELECT", false)).toBe(true);
    expect(blocksSpaceHotkey("DIV", true)).toBe(true);
  });

  it("blocks a focused button, which owns Space as its own activation key", () => {
    // Arming the pan preventDefaults Space; without this the zoom buttons and the
    // pane tabs would silently stop responding to it.
    expect(blocksSpaceHotkey("BUTTON", false)).toBe(true);
  });

  it("blocks anchors and summary elements, which also act on Space", () => {
    expect(blocksSpaceHotkey("A", false)).toBe(true);
    expect(blocksSpaceHotkey("SUMMARY", false)).toBe(true);
  });

  it("allows plain containers and SVG shapes, so a press over the canvas arms", () => {
    expect(blocksSpaceHotkey("DIV", false)).toBe(false);
    expect(blocksSpaceHotkey("BODY", false)).toBe(false);
    expect(blocksSpaceHotkey("g", false)).toBe(false);
    expect(blocksSpaceHotkey("rect", false)).toBe(false);
  });
});
