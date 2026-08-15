/**
 * What a click landed on in the schematic (#267).
 *
 * The pane used to hang two handlers on every glyph — thousands of closures per
 * scope change, all thrown away by the next `innerHTML = ""`. Delegation needs
 * one question answered instead: given the element under the pointer, which
 * model object did the user mean? That question is pure, so it lives here with
 * its own tests; `main.ts` keeps the listeners and the side effects.
 *
 * Resolution is `closest()` over the dataset attributes the renderer already
 * writes, which gives the nesting rule for free: a pin drawn inside its box's
 * group resolves to the pin, because the nearer ancestor wins. That is exactly
 * what the per-element handlers bought with `stopPropagation`.
 */
export type SchemHit =
  | {
      kind: "node";
      id: number;
      /** A pin's own signal path — the context menu prefers it over `id`. */
      probePath?: string;
      /** The box a pin hangs off, probed when the pin has no path of its own. */
      fallbackId?: number;
    }
  | { kind: "wire"; netPath: string; trunkPath?: string };

/** Elements that answer for something. Everything else falls through to null. */
const HIT = "[data-node-id],[data-net-path]";

export function resolveHit(target: Element | null): SchemHit | null {
  const el = target?.closest(HIT);
  if (!el) return null;
  const data = (el as SVGElement | HTMLElement).dataset;

  // A wire carries no node id, so the order of these two branches is not a
  // precedence rule — `closest` already settled which element answers.
  const netPath = data.netPath;
  if (netPath) {
    const hit: SchemHit = { kind: "wire", netPath };
    if (data.trunkPath) hit.trunkPath = data.trunkPath;
    return hit;
  }

  // `Number("")` is 0, so an empty attribute would otherwise select node 0.
  const raw = data.nodeId;
  if (!raw) return null;
  const id = Number(raw);
  if (!Number.isFinite(id)) return null;

  const hit: SchemHit = { kind: "node", id };
  if (data.probePath) hit.probePath = data.probePath;
  if (data.fallbackId) hit.fallbackId = Number(data.fallbackId);
  return hit;
}

/** The net identity a wire element carries, as the renderer stamped it. */
export interface WireData {
  netPath?: string;
  trunkPath?: string;
}

/**
 * Whether a wire lights up for the current selection (#117).
 *
 * A bundle draws as one trunk plus a stub per member, and selecting a member
 * lights the trunk it hangs off but never its sibling stubs — a sibling's own
 * net is not the selection, and the bundle path it shares is only ever matched
 * against `netPath`, never against `trunk`.
 */
export function isWireLit(data: WireData, netPath: string, trunk?: string): boolean {
  return (
    data.netPath === netPath ||
    data.trunkPath === netPath ||
    (trunk !== undefined && data.netPath === trunk)
  );
}
