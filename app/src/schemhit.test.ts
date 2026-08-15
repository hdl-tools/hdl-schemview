// @vitest-environment happy-dom
//
// Per-file, matching `tree.test.ts` and `srcoffset.test.ts`: `resolveHit` walks
// real ancestors with `closest()`, so it needs a DOM. Everything else in the
// suite stays on the default node environment.

import { describe, it, expect } from "vitest";
import { resolveHit, isWireLit } from "./schemhit";

const SVGNS = "http://www.w3.org/2000/svg";

/** Build a detached SVG subtree from a terse spec, returning the deepest node. */
function el(tag: string, data: Record<string, string> = {}, parent?: Element): SVGElement {
  const n = document.createElementNS(SVGNS, tag) as SVGElement;
  for (const [k, v] of Object.entries(data)) n.setAttribute(k, v);
  parent?.appendChild(n);
  return n;
}

describe("resolveHit (#267)", () => {
  it("resolves a box rect to the box's node id", () => {
    const g = el("g");
    const rect = el("rect", { "data-node-id": "42" }, g);

    expect(resolveHit(rect)).toEqual({ kind: "node", id: 42 });
  });

  it("resolves a pin to its own id, not the box it sits inside", () => {
    // The #295 rule that per-element handlers got from stopPropagation: a pin
    // is drawn inside its box's group, and the nearer id must win.
    const box = el("g");
    el("rect", { "data-node-id": "42" }, box);
    const pin = el("circle", { "data-node-id": "1073741824" }, box);

    expect(resolveHit(pin)).toEqual({ kind: "node", id: 1073741824 });
  });

  it("carries a pin's probe path when it has one", () => {
    const pin = el("circle", {
      "data-node-id": "1073741824",
      "data-probe-path": "top.core.rd",
      "data-fallback-id": "42",
    });

    expect(resolveHit(pin)).toEqual({
      kind: "node",
      id: 1073741824,
      probePath: "top.core.rd",
      fallbackId: 42,
    });
  });

  it("falls back to the owning box id when a pin has no path", () => {
    const pin = el("circle", { "data-node-id": "1073741824", "data-fallback-id": "42" });

    expect(resolveHit(pin)).toEqual({ kind: "node", id: 1073741824, fallbackId: 42 });
  });

  it("resolves a wire hit target to its net path", () => {
    const hit = el("polyline", { "data-net-path": "top.core.mem_rdata" });

    expect(resolveHit(hit)).toEqual({ kind: "wire", netPath: "top.core.mem_rdata" });
  });

  it("carries a trunk stub's bundle path alongside its own net (#117)", () => {
    // Selecting a member lights the trunk it hangs off, never its siblings.
    const stub = el("polyline", {
      "data-net-path": "top.core.mem_rdata",
      "data-trunk-path": "top.bus.mem_if",
    });

    expect(resolveHit(stub)).toEqual({
      kind: "wire",
      netPath: "top.core.mem_rdata",
      trunkPath: "top.bus.mem_if",
    });
  });

  it("resolves a label inside a box to that box", () => {
    const g = el("g", { "data-node-id": "42" });
    const text = el("text", {}, g);

    expect(resolveHit(text)).toEqual({ kind: "node", id: 42 });
  });

  it("resolves empty canvas to nothing", () => {
    expect(resolveHit(el("svg"))).toBeNull();
  });

  it("resolves a null target to nothing", () => {
    expect(resolveHit(null)).toBeNull();
  });

  it("ignores a node id that is not a number", () => {
    // Defensive: a malformed attribute must not select node NaN.
    expect(resolveHit(el("rect", { "data-node-id": "" }))).toBeNull();
  });
});

// A bundle (#117) draws as one trunk wire plus one stub per member. Selecting a
// member must light the trunk it hangs off — and nothing else on that trunk.
describe("isWireLit (#267, the #117 rule)", () => {
  const TRUNK = "top.bus.mem_if";
  const MEMBER = "top.core.mem_rdata";
  const SIBLING = "top.core.mem_wdata";

  it("lights the wire whose own net was selected", () => {
    expect(isWireLit({ netPath: MEMBER }, MEMBER)).toBe(true);
  });

  it("lights a member stub when its bundle is the selection", () => {
    expect(isWireLit({ netPath: MEMBER, trunkPath: TRUNK }, TRUNK)).toBe(true);
  });

  it("lights the trunk when one of its members was clicked", () => {
    expect(isWireLit({ netPath: TRUNK }, MEMBER, TRUNK)).toBe(true);
  });

  it("leaves a sibling stub dark when a member is clicked", () => {
    // The rule the whole bundle affordance rests on: clicking one member must
    // not light the rest of the bundle's members.
    expect(isWireLit({ netPath: SIBLING, trunkPath: TRUNK }, MEMBER, TRUNK)).toBe(false);
  });

  it("leaves an unrelated wire dark", () => {
    expect(isWireLit({ netPath: "top.clk" }, MEMBER, TRUNK)).toBe(false);
  });

  it("leaves a wire with no net at all dark", () => {
    expect(isWireLit({}, MEMBER)).toBe(false);
  });
});
