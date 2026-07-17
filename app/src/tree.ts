// The instance-hierarchy tree (#92): the pure `scopeFrames` helper plus `createTree`,
// the DOM factory behind every tree in the app.
//
// This is the one module besides main.ts that touches the DOM — `tree.test.ts` opts
// into happy-dom per-file for it, so the rest (elk/wave/bus/prefs/log) keep Vitest's
// faster DOM-free `node` environment. The factory earns the exception: two trees now
// coexist in one document (the window's #hierarchy tree and the waveform pane's
// signal picker, #171), and the row map that keeps them from colliding is exactly the
// kind of thing worth a test.
//
// The factory takes no session id and never imports `api`: `fetchChildren` is
// injected, so each tree picks its own session (a waveform pop-out's picker queries
// that pane's trace) and this module stays transport-free.

import type { TreeNode } from "./types";

export interface ScopeFrame {
  path: string;
  label: string;
}

/**
 * The breadcrumb frames for a canonical scope path: one frame per dotted
 * segment, each carrying its cumulative path. Segments never contain dots
 * (generate iterations are bracketed, `g_lane[0]`), so a plain split is exact —
 * `picorv32_soc.g_lane[0].memory` → picorv32_soc / g_lane[0] / memory. Used
 * when the tree jumps to an arbitrary scope, so the breadcrumb shows the full
 * ancestor chain instead of the drill-down history.
 */
export function scopeFrames(path: string): ScopeFrame[] {
  const frames: ScopeFrame[] = [];
  let acc = "";
  for (const seg of path.split(".")) {
    acc = acc ? `${acc}.${seg}` : seg;
    frames.push({ path: acc, label: seg });
  }
  return frames;
}

/** A rendered tree, owning its rows. */
export interface TreeHandle {
  /** (Re)build from `top`; drops any rows already rendered. */
  init(top: string): Promise<void>;
  /** Move the `.sel` highlight to `path`; clears it when that row isn't rendered. */
  highlight(path: string): void;
  /** Drop every row (no design loaded). */
  clear(): void;
}

export interface TreeOptions {
  /** The element the tree renders into. */
  host: HTMLElement;
  /**
   * Fetch a scope's subtree. Injected rather than importing `api`, so this module
   * stays transport-free and each tree names its own session — the window's tree
   * passes the main session, a waveform pane's picker passes that pane's.
   */
  fetchChildren: (path: string, depth: number) => Promise<TreeNode>;
  /** Single-click a row. */
  onSelect: (node: TreeNode) => void;
  /** Double-click a row; omit to leave dblclick unwired. */
  onActivate?: (node: TreeNode) => void;
}

/**
 * Render a lazy hierarchy tree into `host`. Each call owns its own row map, so
 * several trees coexist in one document without fighting over the highlight (#171):
 * a module-level map would let one tree's `init` orphan another's rows.
 */
export function createTree(opts: TreeOptions): TreeHandle {
  const { host, fetchChildren, onSelect, onActivate } = opts;
  // Rendered rows keyed by scope path, for selection-highlight sync.
  const treeItems = new Map<string, HTMLElement>();

  function treeItem(node: TreeNode): HTMLLIElement {
    const li = document.createElement("li");
    const row = document.createElement("div");
    row.className = "tnode";
    const twist = document.createElement("span");
    twist.className = "twist";
    const label = document.createElement("span");
    label.className = "tlabel";
    label.textContent = node.label;
    row.appendChild(twist);
    row.appendChild(label);
    // Module sublabel, unless it repeats the label (e.g. the design top).
    if (node.module && node.module !== node.label) {
      const mod = document.createElement("span");
      mod.className = "tmod";
      mod.textContent = `(${node.module})`;
      row.appendChild(mod);
    }
    li.appendChild(row);
    treeItems.set(node.path, row);

    let kids: HTMLUListElement | null = null;
    const attachKids = (children: TreeNode[]) => {
      const ul = document.createElement("ul");
      for (const c of children) ul.appendChild(treeItem(c));
      li.appendChild(ul);
      kids = ul;
      return ul;
    };
    if (node.children.length) attachKids(node.children);

    let open = node.children.length > 0;
    const setOpen = (o: boolean) => {
      open = o;
      twist.textContent = node.expandable ? (open ? "▾" : "▸") : "";
      if (kids) kids.style.display = open ? "" : "none";
    };
    setOpen(open);
    twist.onclick = async (e) => {
      e.stopPropagation();
      if (!node.expandable) return;
      if (!kids) {
        // Lazy: fetch this node's direct children on first expand. Reserve the
        // list synchronously so a second click during the fetch can't attach a
        // duplicate one.
        const ul = attachKids([]);
        const sub = await fetchChildren(node.path, 1);
        for (const c of sub.children) ul.appendChild(treeItem(c));
      }
      setOpen(!open);
    };
    row.onclick = () => onSelect(node);
    if (onActivate) {
      row.ondblclick = (e) => {
        e.stopPropagation();
        onActivate(node);
      };
    }
    return li;
  }

  function highlight(path: string) {
    for (const [p, el] of treeItems) el.classList.toggle("sel", p === path);
  }

  function clear() {
    host.innerHTML = "";
    treeItems.clear();
  }

  return {
    async init(top: string) {
      clear();
      const root = await fetchChildren(top, 1);
      const ul = document.createElement("ul");
      ul.appendChild(treeItem(root));
      host.appendChild(ul);
      highlight(top);
    },
    highlight,
    clear,
  };
}
