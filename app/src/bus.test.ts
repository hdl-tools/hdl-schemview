import { describe, it, expect } from "vitest";
import {
  crossProbeSelection,
  ownsSelection,
  paneModeOf,
  scopeSelection,
  SELF,
} from "./bus";
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
    expect(sel.dest).toBeNull();
  });
  it("supports multiple targets and a custom origin", () => {
    const sel = crossProbeSelection(resp, ["source", "waveform"], "schematic");
    expect(sel.targets).toEqual(["source", "waveform"]);
    expect(sel.origin).toBe("schematic");
  });
  it("can address a specific destination window", () => {
    const sel = crossProbeSelection(resp, ["schematic"], "main", "schematic-2");
    expect(sel.dest).toBe("schematic-2");
  });
});

describe("scopeSelection", () => {
  it("targets the schematic with a scope path and no response", () => {
    const sel = scopeSelection("top.u");
    expect(sel.scope).toBe("top.u");
    expect(sel.targets).toEqual(["schematic"]);
    expect(sel.resp).toBeNull();
    expect(sel.origin).toBe(SELF);
    expect(sel.dest).toBeNull();
  });
  it("can address a specific destination window", () => {
    const sel = scopeSelection("top.u", "main", "schematic-2");
    expect(sel.dest).toBe("schematic-2");
  });
});

describe("paneModeOf", () => {
  it("reads a detached pane from the query", () => {
    expect(paneModeOf("?pane=schematic")).toBe("schematic");
    expect(paneModeOf("?pane=waveform")).toBe("waveform");
  });
  it("defaults to main for no/unknown pane", () => {
    expect(paneModeOf("")).toBe("main");
    expect(paneModeOf("?pane=bogus")).toBe("main");
    expect(paneModeOf("?other=1")).toBe("main");
  });
});

describe("ownsSelection", () => {
  const main = { mode: "main" as const, self: "main" };
  const wave1 = { mode: "waveform" as const, self: "waveform-1" };
  const wave2 = { mode: "waveform" as const, self: "waveform-2" };
  const schem1 = { mode: "schematic" as const, self: "schematic-1" };
  const schem2 = { mode: "schematic" as const, self: "schematic-2" };

  it("source is owned only by the main window", () => {
    expect(ownsSelection(main, "source", null, [])).toBe(true);
    expect(ownsSelection(schem1, "source", null, [])).toBe(false);
    expect(ownsSelection(wave1, "source", null, [])).toBe(false);
  });

  it("a broadcast waveform selection drives only the main window", () => {
    // #170: waveform detach is independent, not a mirror — main keeps its own
    // waveform even while pop-outs exist, and pop-outs never follow a broadcast.
    expect(ownsSelection(main, "waveform", null, [])).toBe(true);
    expect(ownsSelection(wave1, "waveform", null, [])).toBe(false);
    expect(ownsSelection(wave2, "waveform", null, [])).toBe(false);
  });

  it("an addressed waveform selection goes to exactly that window", () => {
    expect(ownsSelection(wave2, "waveform", "waveform-2", [])).toBe(true);
    expect(ownsSelection(wave1, "waveform", "waveform-2", [])).toBe(false);
    expect(ownsSelection(main, "waveform", "waveform-2", [])).toBe(false);
    // main is addressable by its own label.
    expect(ownsSelection(main, "waveform", "main", [])).toBe(true);
    expect(ownsSelection(wave1, "waveform", "main", [])).toBe(false);
  });

  it("a broadcast schematic selection drives only the main window", () => {
    // #169: schematic detach is independent, not a mirror — main keeps its own
    // schematic even while pop-outs exist, and pop-outs never follow a broadcast.
    expect(ownsSelection(main, "schematic", null, ["schematic"])).toBe(true);
    expect(ownsSelection(schem1, "schematic", null, [])).toBe(false);
    expect(ownsSelection(schem2, "schematic", null, [])).toBe(false);
  });

  it("an addressed schematic selection goes to exactly that window", () => {
    expect(ownsSelection(schem2, "schematic", "schematic-2", [])).toBe(true);
    expect(ownsSelection(schem1, "schematic", "schematic-2", [])).toBe(false);
    expect(ownsSelection(main, "schematic", "schematic-2", [])).toBe(false);
  });
});
