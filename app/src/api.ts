// Typed wrappers over the Tauri commands exposed by app/src-tauri.
import { invoke } from "@tauri-apps/api/core";
import type { ProbeResponse, SchematicGraph, TraceTimescale, TreeNode, ValueChange } from "./types";

export const api = {
  loadDesign: (model: string, trace: string, excluded: string[], srcRoot: string) =>
    invoke<string>("load_design", { model, trace, excluded, srcRoot }),
  elaborateAndLoad: (
    filelist: string,
    top: string,
    incdirs: string[],
    trace: string,
    excluded: string[],
    srcRoot: string,
  ) => invoke<string>("elaborate_and_load", { filelist, top, incdirs, trace, excluded, srcRoot }),

  scopeGraph: (scope: string) => invoke<SchematicGraph>("scope_graph", { scope }),
  expandNode: (node: number) => invoke<SchematicGraph>("expand_node", { node }),
  hierarchyTree: (scope: string, depth: number) =>
    invoke<TreeNode>("hierarchy_tree", { scope, depth }),
  cone: (net: number, dir: string, depth: number) =>
    invoke<SchematicGraph>("cone", { net, dir, depth }),

  probeNode: (path: string, context: string | null) =>
    invoke<ProbeResponse | null>("probe_node", { path, context }),
  probeSignal: (fullName: string, context: string | null) =>
    invoke<ProbeResponse | null>("probe_signal", { fullName, context }),
  probeSource: (file: number, offset: number, context: string | null) =>
    invoke<ProbeResponse | null>("probe_source", { file, offset, context }),

  signalValues: (signalRef: number) =>
    invoke<ValueChange[]>("signal_values", { signalRef }),
  sourceText: (file: number) => invoke<string>("source_text", { file }),
  traceTimescale: () => invoke<TraceTimescale | null>("trace_timescale", {}),
};
