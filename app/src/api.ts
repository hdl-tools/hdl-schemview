// Typed wrappers over the Tauri commands exposed by app/src-tauri.
//
// Multi-session (#168): every command accepts an optional trailing `sessionId`.
// Omitting it (all existing callers) targets the backend's "main" session, so
// single-session behavior is unchanged; an independent window passes its own id
// to load/query a separate design + trace. `undefined` is dropped from the invoke
// payload, so the Rust `Option<String>` resolves to the default.
import { invoke } from "@tauri-apps/api/core";
import type {
  ProbeResponse,
  SchematicGraph,
  StartupArgs,
  TraceTimescale,
  TreeNode,
  ValueChange,
} from "./types";

export const api = {
  loadDesign: (
    model: string,
    trace: string,
    excluded: string[],
    srcRoot: string,
    sessionId?: string,
  ) => invoke<string>("load_design", { sessionId, model, trace, excluded, srcRoot }),
  elaborateAndLoad: (
    filelist: string,
    top: string,
    incdirs: string[],
    trace: string,
    excluded: string[],
    srcRoot: string,
    sessionId?: string,
  ) =>
    invoke<string>("elaborate_and_load", {
      sessionId,
      filelist,
      top,
      incdirs,
      trace,
      excluded,
      srcRoot,
    }),
  // Swap a session's trace, keeping its design (#176) — far cheaper than a full
  // loadDesign/elaborateAndLoad, which re-ingests (or re-elaborates) an unchanged design.
  loadTrace: (trace: string, sessionId?: string) =>
    invoke<void>("load_trace", { sessionId, trace }),
  unloadDesign: (sessionId?: string) => invoke<void>("unload_design", { sessionId }),

  scopeGraph: (scope: string, sessionId?: string) =>
    invoke<SchematicGraph>("scope_graph", { sessionId, scope }),
  expandNode: (node: number, sessionId?: string) =>
    invoke<SchematicGraph>("expand_node", { sessionId, node }),
  hierarchyTree: (scope: string, depth: number, sessionId?: string) =>
    invoke<TreeNode>("hierarchy_tree", { sessionId, scope, depth }),
  cone: (net: number, dir: string, depth: number, sessionId?: string) =>
    invoke<SchematicGraph>("cone", { sessionId, net, dir, depth }),

  probeNode: (path: string, context: string | null, sessionId?: string) =>
    invoke<ProbeResponse | null>("probe_node", { sessionId, path, context }),
  probeSignal: (fullName: string, context: string | null, sessionId?: string) =>
    invoke<ProbeResponse | null>("probe_signal", { sessionId, fullName, context }),
  probeSource: (file: number, offset: number, context: string | null, sessionId?: string) =>
    invoke<ProbeResponse | null>("probe_source", { sessionId, file, offset, context }),

  signalValues: (signalRef: number, sessionId?: string) =>
    invoke<ValueChange[]>("signal_values", { sessionId, signalRef }),
  sourceText: (file: number, sessionId?: string) =>
    invoke<string>("source_text", { sessionId, file }),
  traceTimescale: (sessionId?: string) =>
    invoke<TraceTimescale | null>("trace_timescale", { sessionId }),
  startupArgs: () => invoke<StartupArgs | null>("startup_args", {}),
};
