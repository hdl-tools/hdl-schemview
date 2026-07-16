// bus.ts — the single selection channel (#18). Cross-pane coordination
// (schematic ↔ source ↔ waveform, and tree → schematic) flows through one
// publish/subscribe bus instead of direct calls, so the same wiring drives the
// panes whether they share one window or are detached into their own (Tauri
// multi-window, #18 PR2). The transport is Tauri's app-global emit/listen when
// running inside the webview, and a module-local fan-out otherwise (browser dev
// server / unit tests) — one logical channel, two transports, chosen by
// environment. The Selection payload carries the model's *resolved* cross-probe
// result (or a scope path to drill into), never rendered geometry, so every
// subscriber drives its panes from a model lookup — the single-source-of-truth
// invariant, unchanged across the window boundary.
import type { ProbeResponse } from "./types";

// The Tauri event name the bus rides on.
export const SELECT_EVENT = "xprobe:select";

// Which panes a selection should drive. A menu action targets exactly the pane it
// names ("Show in source" → ["source"]); a whole-object select could target more.
export type RevealTarget = "source" | "schematic" | "waveform";

// One cross-pane selection. Exactly one of `resp` (a resolved cross-probe: source
// location, wave link, schematic anchor + ambiguity alternatives) or `scope` (a
// scope path to drill the schematic into, from a tree click) is set; `targets`
// says which panes act on it. `origin` tags the publishing window. `dest`
// optionally addresses one specific window by label (#169) — `null` broadcasts to
// the default owner (main's own panes). With independent schematic panes, a
// broadcast schematic selection drives only main's schematic; pop-outs are
// self-driven and ignore it unless a selection is addressed to them by label.
export interface Selection {
  origin: string;
  targets: RevealTarget[];
  resp: ProbeResponse | null;
  scope: string | null;
  dest: string | null;
}

// Default origin/label of the main window. Detached windows stamp their own
// unique label (#169) as `origin`.
export const SELF = "main";

// Build a selection that reveals a resolved cross-probe in the named panes.
export function crossProbeSelection(
  resp: ProbeResponse,
  targets: RevealTarget[],
  origin: string = SELF,
  dest: string | null = null,
): Selection {
  return { origin, targets, resp, scope: null, dest };
}

// Build a selection that drills the schematic into a scope path (a tree jump).
export function scopeSelection(
  scope: string,
  origin: string = SELF,
  dest: string | null = null,
): Selection {
  return { origin, targets: ["schematic"], resp: null, scope, dest };
}

// -- window modes (#18 PR2) -------------------------------------------------

// What a webview hosts: the full app ("main"), or a single detached pane.
export type PaneMode = "main" | "schematic" | "waveform";

// Read the pane mode from a window's URL query (`?pane=schematic`). A detached
// window is launched with this param; everything else is the main window.
export function paneModeOf(search: string): PaneMode {
  const p = new URLSearchParams(search).get("pane");
  return p === "schematic" || p === "waveform" ? p : "main";
}

// A window's identity for ownership decisions: its pane `mode` (main / schematic /
// waveform) and its unique `self` label (main = "main", pop-outs = "schematic-2",
// "waveform-1"…).
export interface WindowId {
  mode: PaneMode;
  self: string;
}

// Whether `win` should drive `target` for an incoming selection addressed to
// `dest` (null = broadcast). `detached` is retained for API compatibility but only
// consulted by callers; ownership is now purely id/dest keyed for the pane views.
//
// - `source`: only the main window hosts it.
// - `waveform`: independent model (#170) — an addressed selection drives exactly the
//   window whose label matches `dest` (main = "main", pop-outs = "waveform-1"…); a
//   broadcast drives only main's own waveform. Pop-out waveform windows are
//   self-driven and never follow a broadcast.
// - `schematic`: independent model (#169) — an addressed selection drives exactly
//   the window whose label matches `dest`; a broadcast selection drives only
//   main's own schematic. Pop-out schematic windows are self-driven (local drill)
//   and never follow a broadcast, so selecting in one pane can't move another.
export function ownsSelection(
  win: WindowId,
  target: RevealTarget,
  dest: string | null,
  _detached: readonly RevealTarget[],
): boolean {
  switch (target) {
    case "source":
      return win.mode === "main";
    case "waveform":
    case "schematic":
      return dest !== null ? win.self === dest : win.mode === "main";
  }
}

// -- transport --------------------------------------------------------------

export type Unsubscribe = () => void;

// Tauri injects __TAURI_INTERNALS__ into every webview; its absence means a plain
// browser (the Vite dev server) or a test runner, where we fan out locally so
// same-window cross-probing still works without the IPC bridge.
const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

// Local-transport subscribers (browser dev / tests), delivered to synchronously.
const localSubs = new Set<(sel: Selection) => void>();

// Publish a selection to every window's subscriber (Tauri) or every local
// subscriber (browser/tests). Never rejects — a transport failure is logged, not
// thrown, so a fire-and-forget caller can't leak an unhandled rejection.
export async function publish(sel: Selection): Promise<void> {
  try {
    if (inTauri) {
      const { emit } = await import("@tauri-apps/api/event");
      await emit(SELECT_EVENT, sel);
    } else {
      for (const fn of localSubs) fn(sel);
    }
  } catch (e) {
    console.warn("selection publish failed:", e);
  }
}

// Subscribe to selections; returns an unsubscribe handle. On Tauri this listens on
// the app-global event, so it also receives selections emitted by other windows.
export async function subscribe(
  handler: (sel: Selection) => void,
): Promise<Unsubscribe> {
  if (inTauri) {
    const { listen } = await import("@tauri-apps/api/event");
    return await listen<Selection>(SELECT_EVENT, (e) => handler(e.payload));
  }
  localSubs.add(handler);
  return () => localSubs.delete(handler);
}
