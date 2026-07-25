// Semantic name coloring (#225). Overlays the model's identifier-occurrence spans
// (the backend `NameRefDto`) onto the lexical token rows from syntax.ts: each ref
// splits the `plain` token it lands in into a `name-<class>` token, so the lexer stays
// authoritative for keywords/comments/strings and the model authoritative for
// identifiers. **Only `plain` tokens are split** — that is what keeps the two layers
// from overwriting each other (a keyword the lexer coloured is never re-tinted, and an
// identifier the model didn't classify stays plain rather than guessed at).
//
// Invariant (shared with syntax.ts / srcoffset.ts): a line's tokens still concatenate
// back to the original line exactly — `lineColumn`, and thus the whole byte-offset
// cross-probe, depend on it. Unit-tested here.

import type { Token } from "./syntax";

/** One identifier occurrence from the backend (`NameRefDto`); `line`/`col` are 1-based. */
export interface NameRef {
  line: number;
  col: number;
  len: number;
  /** The `NameClass`, kebab-cased (`signal`, `enum-member`, …). Becomes `name-<cls>`. */
  cls: string;
}

/** A rendered token: syntax.ts's `Token` widened to carry the `name-<class>` classes. */
export interface RenderToken {
  text: string;
  cls: string;
}

/**
 * Split each line's `plain` tokens at the identifier spans for that line, tagging the
 * covered slice `name-<cls>`. `refs` is the whole file's ref list; it is grouped by line
 * once. Source is ASCII (RTL identifiers are), so a 1-based byte `col` equals a JS
 * string index — the same basis `syntax.ts` and `srcoffset.ts` already assume.
 */
export function applyNameRefs(lines: Token[][], refs: NameRef[]): RenderToken[][] {
  if (!refs.length) return lines;
  const byLine = new Map<number, NameRef[]>();
  for (const r of refs) {
    const arr = byLine.get(r.line);
    if (arr) arr.push(r);
    else byLine.set(r.line, [r]);
  }
  return lines.map((toks, i) => {
    const onLine = byLine.get(i + 1);
    return onLine ? splitLine(toks, onLine) : toks;
  });
}

// Overlay one line's refs onto its tokens. `col` tracks the 0-based column at each
// token's start (== the running character count), so a ref's line-column maps straight
// into token-local slice indices.
function splitLine(toks: Token[], refs: NameRef[]): RenderToken[] {
  const sorted = [...refs].sort((a, b) => a.col - b.col);
  const out: RenderToken[] = [];
  let col = 0;
  for (const t of toks) {
    const start = col;
    const end = col + t.text.length;
    col = end;
    if (t.cls !== "plain") {
      out.push(t);
      continue;
    }
    // Refs fully inside this plain token, in column order.
    const hits = sorted.filter((r) => r.col - 1 >= start && r.col - 1 + r.len <= end);
    if (!hits.length) {
      out.push(t);
      continue;
    }
    let pos = start;
    for (const r of hits) {
      const s = r.col - 1;
      const e = s + r.len;
      if (s < pos) continue; // overlaps an already-emitted ref — skip (keeps concat exact)
      if (s > pos) out.push({ text: t.text.slice(pos - start, s - start), cls: "plain" });
      out.push({ text: t.text.slice(s - start, e - start), cls: "name-" + r.cls });
      pos = e;
    }
    if (pos < end) out.push({ text: t.text.slice(pos - start), cls: "plain" });
  }
  return out;
}
