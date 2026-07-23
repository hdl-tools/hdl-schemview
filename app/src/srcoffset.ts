// The caret→line-column DOM walk (#223), extracted from main.ts's `sourceOffsetAt` so the
// part that must survive inline highlight spans (#223/#224) is unit-tested (happy-dom).
//
// Before highlighting, a rendered line held exactly one text node, so the caret's offset
// within that node *was* the column. Now a line is a run of token nodes (bare text for
// `plain`, `<span class="tok-…">` otherwise), so the caret offset is relative to one token.
// `lineColumn` recovers the true column by summing the text length of the token nodes
// before the caret's, keeping `lineStarts[line] + column` — the byte offset the source
// cross-probe resolves through — correct.

// The line-number gutter span rendered by `renderSourceInto`; not part of the code text.
const GUTTER = "ln";

export function lineColumn(lineDiv: Element, node: Node, offsetInNode: number): number {
  // The direct child of `lineDiv` containing `node`: a bare text token, or the
  // <span class="tok-…"> wrapping one.
  let top: Node = node;
  while (top.parentNode && top.parentNode !== lineDiv) top = top.parentNode;
  let preceding = 0;
  for (let sib = top.previousSibling; sib; sib = sib.previousSibling) {
    if (sib.nodeType === Node.ELEMENT_NODE && (sib as Element).classList.contains(GUTTER)) {
      continue;
    }
    preceding += sib.textContent?.length ?? 0;
  }
  return preceding + offsetInNode;
}
