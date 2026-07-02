// Pure helpers for the instance-hierarchy tree pane (#92). DOM-free so they
// unit-test under Vitest like elk.ts / wave.ts.

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
