import { describe, expect, it } from "vitest";

import { formatLogEntry, formatTime } from "./log";

describe("formatTime", () => {
  it("formats a local time as zero-padded HH:MM:SS", () => {
    expect(formatTime(new Date(2026, 6, 9, 9, 5, 3))).toBe("09:05:03");
  });

  it("keeps a 24-hour clock", () => {
    expect(formatTime(new Date(2026, 6, 9, 23, 59, 7))).toBe("23:59:07");
  });
});

describe("formatLogEntry", () => {
  it("assembles the timestamp, level, and message", () => {
    expect(
      formatLogEntry("error", "load failed", new Date(2026, 6, 9, 14, 2, 30)),
    ).toEqual({ ts: "14:02:30", level: "error", message: "load failed" });
  });
});
