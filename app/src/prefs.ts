// User preferences (#17) — the lightweight first surface of the roadmap's Phase 6
// "config system". The parse/coerce helpers are DOM-free so they unit-test under
// Vitest like log.ts / tree.ts; the load*/save* wrappers just bridge them to
// localStorage (the same per-key pattern as waveCol / bottomRowH / treeColW), and
// are only called from main.ts. Theme keeps its own existing "theme" key/mechanism.

/** Scopes excluded from a design load by default (testbench + package wrappers). */
export const DEFAULT_EXCLUDED = ["TOP", "tb", "soc_pkg"];

const KEY_EXCLUDED = "excludedScopes";

/** Split an editor string into scope names on commas / whitespace, dropping empties. */
export function parseExcluded(text: string): string[] {
  return text
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

/** Render a scope list back into the comma-separated form the editor shows. */
export function formatExcluded(scopes: string[]): string {
  return scopes.join(", ");
}

// Interpret a persisted `excludedScopes` value, falling back to a fresh copy of the
// default on a missing / malformed / non-string-array payload. An empty array is a
// legitimate user choice ("exclude nothing") and is preserved, not defaulted.
export function coerceExcluded(raw: string | null): string[] {
  if (raw !== null) {
    try {
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed) && parsed.every((s) => typeof s === "string")) {
        return parsed;
      }
    } catch {
      /* malformed JSON — fall through to the default */
    }
  }
  return [...DEFAULT_EXCLUDED];
}

export function loadExcluded(): string[] {
  try {
    return coerceExcluded(localStorage.getItem(KEY_EXCLUDED));
  } catch {
    return [...DEFAULT_EXCLUDED];
  }
}

export function saveExcluded(scopes: string[]): void {
  try {
    localStorage.setItem(KEY_EXCLUDED, JSON.stringify(scopes));
  } catch {
    /* ignore persistence failure */
  }
}

// Gate-level schematic projection toggle (#157). Off by default, so the schematic
// stays byte-identical to the process-level view until the user opts in.
const KEY_GATE_LEVEL = "gateLevel";

export function loadGateLevel(): boolean {
  try {
    return localStorage.getItem(KEY_GATE_LEVEL) === "true";
  } catch {
    return false;
  }
}

export function saveGateLevel(on: boolean): void {
  try {
    localStorage.setItem(KEY_GATE_LEVEL, String(on));
  } catch {
    /* ignore persistence failure */
  }
}

// Semantic name coloring toggle (#225). On by default — the source pane colors
// identifiers by the model's classification. Off falls back to lexical-only
// highlighting (#223). Stored as the literal "false" so an absent key reads as on.
const KEY_SEMANTIC_NAMES = "semanticNames";

export function loadSemanticNames(): boolean {
  try {
    return localStorage.getItem(KEY_SEMANTIC_NAMES) !== "false";
  } catch {
    return true;
  }
}

export function saveSemanticNames(on: boolean): void {
  try {
    localStorage.setItem(KEY_SEMANTIC_NAMES, String(on));
  } catch {
    /* ignore persistence failure */
  }
}
