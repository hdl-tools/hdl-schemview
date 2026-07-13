// Pure helpers for the source pane (#158). DOM-free so they unit-test under
// Vitest like log.ts / tree.ts / wave.ts; main.ts's renderSource applies the
// result to the per-line divs.

/**
 * Index of the line containing byte `offset`: the largest `i` with
 * `lineStarts[i] <= offset`, clamped into `[0, lineStarts.length - 1]`.
 * `lineStarts` is the monotonically increasing byte offset of each line start.
 */
function lineForOffset(lineStarts: number[], offset: number): number {
  let lo = 0;
  let hi = lineStarts.length - 1;
  let ans = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (lineStarts[mid] <= offset) {
      ans = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return ans;
}

/**
 * Inclusive `[startLine, endLine]` line-index range covering the byte span
 * `[startOffset, endOffset)` (slang's `end` is exclusive). The last highlighted
 * line is derived from `endOffset - 1` so an exclusive end sitting exactly on a
 * line boundary does not spill onto the next line. A degenerate/point span
 * (`endOffset <= startOffset`) collapses to the single start line.
 */
export function lineRangeForSpan(
  lineStarts: number[],
  startOffset: number,
  endOffset: number,
): [number, number] {
  if (lineStarts.length === 0) return [0, 0];
  const start = lineForOffset(lineStarts, startOffset);
  if (endOffset <= startOffset) return [start, start];
  const end = lineForOffset(lineStarts, endOffset - 1);
  return [start, end < start ? start : end];
}
