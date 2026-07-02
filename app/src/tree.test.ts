import { describe, expect, it } from "vitest";

import { scopeFrames } from "./tree";

describe("scopeFrames", () => {
  it("builds one frame per segment with cumulative paths", () => {
    expect(scopeFrames("picorv32_soc.g_lane[0].memory")).toEqual([
      { path: "picorv32_soc", label: "picorv32_soc" },
      { path: "picorv32_soc.g_lane[0]", label: "g_lane[0]" },
      { path: "picorv32_soc.g_lane[0].memory", label: "memory" },
    ]);
  });

  it("handles the top scope", () => {
    expect(scopeFrames("picorv32_soc")).toEqual([
      { path: "picorv32_soc", label: "picorv32_soc" },
    ]);
  });
});
