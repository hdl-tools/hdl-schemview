import { describe, it, expect } from "vitest";
import { crossProbeSelection, scopeSelection, SELF } from "./bus";
import type { ProbeResponse } from "./types";

const resp: ProbeResponse = {
  anchor: { id: 1, path: "top.u.sig", kind: "Net" },
  source: null,
  wave: { in_trace: false, var_ref: 0, signal_ref: 0, full_name: "top.u.sig" },
  alternatives: [],
};

describe("crossProbeSelection", () => {
  it("carries the resolved response and the named target panes", () => {
    const sel = crossProbeSelection(resp, ["source"]);
    expect(sel.resp).toBe(resp);
    expect(sel.targets).toEqual(["source"]);
    expect(sel.scope).toBeNull();
    expect(sel.origin).toBe(SELF);
  });
  it("supports multiple targets and a custom origin", () => {
    const sel = crossProbeSelection(resp, ["source", "waveform"], "schematic");
    expect(sel.targets).toEqual(["source", "waveform"]);
    expect(sel.origin).toBe("schematic");
  });
});

describe("scopeSelection", () => {
  it("targets the schematic with a scope path and no response", () => {
    const sel = scopeSelection("top.u");
    expect(sel.scope).toBe("top.u");
    expect(sel.targets).toEqual(["schematic"]);
    expect(sel.resp).toBeNull();
    expect(sel.origin).toBe(SELF);
  });
});
