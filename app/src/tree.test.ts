// @vitest-environment happy-dom
//
// Per-file, not a vitest.config: `createTree` is the only DOM code outside main.ts,
// so every other suite keeps the faster DOM-free `node` environment.
import { describe, expect, it, vi } from "vitest";

import { createTree, scopeFrames } from "./tree";
import type { TreeNode } from "./types";

const node = (
  path: string,
  children: TreeNode[] = [],
  expandable = false,
): TreeNode => ({
  label: path.split(".").pop()!,
  path,
  expandable: expandable || children.length > 0,
  children,
});

function host(): HTMLElement {
  const el = document.createElement("div");
  document.body.appendChild(el);
  return el;
}

/**
 * Fire a row/twist handler and hand back its promise, so a test can await the lazy
 * fetch. The DOM lib types `onclick` as taking a PointerEvent; happy-dom's MouseEvent
 * is what a real click delivers here and the handlers only read `stopPropagation`.
 */
const click = (el: HTMLElement) =>
  el.onclick!(new MouseEvent("click") as unknown as PointerEvent);

/** The rendered row for a path, or undefined. */
const rowFor = (h: HTMLElement, path: string): Element | undefined =>
  [...h.querySelectorAll(".tnode")].find(
    (r) => r.querySelector(".tlabel")?.textContent === path.split(".").pop(),
  );

describe("scopeFrames", () => {
  it("builds one frame per segment with cumulative paths", () => {
    expect(scopeFrames("picorv32_soc.g_lane[0].memory")).toEqual([
      { path: "picorv32_soc", label: "picorv32_soc" },
      { path: "picorv32_soc.g_lane[0]", label: "g_lane[0]" },
      { path: "picorv32_soc.g_lane[0].memory", label: "memory" },
    ]);
  });

  it("handles the top scope", () => {
    expect(scopeFrames("picorv32_soc")).toEqual([
      { path: "picorv32_soc", label: "picorv32_soc" },
    ]);
  });
});

describe("createTree", () => {
  const top = () =>
    node("soc", [node("soc.cpu", [], true), node("soc.mem", [], true)]);

  // The reason the factory exists: the window's #hierarchy tree and a waveform pane's
  // signal picker (#171) render into one document. When the row map was module-level,
  // one tree's highlight lit the other's rows and its `init` orphaned them.
  it("keeps two trees in one document from colliding", async () => {
    const [ha, hb] = [host(), host()];
    const fetch = () => Promise.resolve(top());
    const a = createTree({ host: ha, fetchChildren: fetch, onSelect: () => {} });
    const b = createTree({ host: hb, fetchChildren: fetch, onSelect: () => {} });
    await a.init("soc");
    await b.init("soc");

    a.highlight("soc.cpu");

    // A's own row lights up...
    expect(rowFor(ha, "soc.cpu")!.classList.contains("sel")).toBe(true);
    // ...and B is untouched, still showing the root its own init selected. With a
    // shared row map both assertions invert: B's rows would win the map and take the
    // highlight, while A's — overwritten and orphaned — would never light at all.
    expect(rowFor(hb, "soc.cpu")!.classList.contains("sel")).toBe(false);
    expect(rowFor(hb, "soc")!.classList.contains("sel")).toBe(true);
  });

  it("fetches the root at depth 1, then each child lazily on expand", async () => {
    const fetchChildren = vi.fn(async (path: string) =>
      path === "soc" ? top() : node(path, [node(`${path}.regs`)]),
    );
    const h = host();
    const t = createTree({ host: h, fetchChildren, onSelect: () => {} });
    await t.init("soc");
    expect(fetchChildren.mock.calls).toEqual([["soc", 1]]);

    const twist = rowFor(h, "soc.cpu")!.querySelector(".twist") as HTMLElement;
    await click(twist);

    expect(fetchChildren.mock.calls).toEqual([
      ["soc", 1],
      ["soc.cpu", 1],
    ]);
    expect(rowFor(h, "soc.cpu.regs")).toBeTruthy();
  });

  // The child list is reserved synchronously, so a second click landing mid-fetch
  // can't attach a duplicate one.
  it("expands once when clicked twice during the fetch", async () => {
    let release: (n: TreeNode) => void = () => {};
    const fetchChildren = vi.fn((path: string) =>
      path === "soc"
        ? Promise.resolve(top())
        : new Promise<TreeNode>((r) => (release = r)),
    );
    const h = host();
    const t = createTree({ host: h, fetchChildren, onSelect: () => {} });
    await t.init("soc");

    const twist = rowFor(h, "soc.cpu")!.querySelector(".twist") as HTMLElement;
    const first = click(twist);
    const second = click(twist);
    release(node("soc.cpu", [node("soc.cpu.regs")]));
    await Promise.all([first, second]);

    expect(fetchChildren).toHaveBeenCalledTimes(2); // root + one expand
    expect(h.querySelectorAll("li")).toHaveLength(4); // soc, cpu, mem, regs — no dupe
  });

  it("reports the clicked node to onSelect", async () => {
    const onSelect = vi.fn();
    const h = host();
    const t = createTree({
      host: h,
      fetchChildren: () => Promise.resolve(top()),
      onSelect,
    });
    await t.init("soc");

    click(rowFor(h, "soc.cpu") as HTMLElement);

    expect(onSelect.mock.calls[0][0].path).toBe("soc.cpu");
  });

  // The picker passes no onActivate — a double-click there must be inert, not a crash.
  it("leaves dblclick unwired when onActivate is omitted", async () => {
    const h = host();
    const t = createTree({
      host: h,
      fetchChildren: () => Promise.resolve(top()),
      onSelect: () => {},
    });
    await t.init("soc");

    expect((rowFor(h, "soc.cpu") as HTMLElement).ondblclick).toBeNull();
  });

  it("drops the old rows on re-init and on clear", async () => {
    const h = host();
    const t = createTree({
      host: h,
      fetchChildren: () => Promise.resolve(top()),
      onSelect: () => {},
    });
    await t.init("soc");
    await t.init("soc");
    expect(h.querySelectorAll("li")).toHaveLength(3); // not 6

    t.clear();
    expect(h.innerHTML).toBe("");
    // The map cleared too, so a stale path can't resurrect a highlight.
    expect(() => t.highlight("soc.cpu")).not.toThrow();
  });
});
