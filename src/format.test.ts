import { describe, it, expect } from "vitest";
import { tileLabel, generatedLabel, splitReset } from "./format";

describe("tileLabel", () => {
  it("returns 'Session' for a bucket named 'Session'", () => {
    expect(tileLabel("Session")).toBe("Session");
  });

  it("returns 'Session' for names starting with 'Session'", () => {
    expect(tileLabel("Session 5-hour")).toBe("Session");
  });

  it("returns 'Week' for the all-models weekly bucket", () => {
    expect(tileLabel("Week · all models")).toBe("Week");
  });

  it("returns '<model> wk' for a model-scoped weekly bucket", () => {
    expect(tileLabel("Week · Opus")).toBe("Opus wk");
    expect(tileLabel("Week · Sonnet")).toBe("Sonnet wk");
  });

  it("appends ' wk' to unrecognized names (fallback)", () => {
    // Any non-Session, non-all-models name gets " wk" appended.
    expect(tileLabel("Custom bucket")).toBe("Custom bucket wk");
    // A name with no scope after "·" returns the original.
    expect(tileLabel("Week · ")).toBe("Week · ");
  });
});

describe("generatedLabel", () => {
  it("formats epoch ms as a local date-time string", () => {
    // 2026-01-15T10:30:00.000Z in UTC → local time depends on timezone,
    // but the epoch is deterministic so we can check the format shape.
    const ms = Date.UTC(2026, 0, 15, 10, 30, 0);
    const label = generatedLabel(ms, "fallback");
    // Should be a YYYY-MM-DD HH:MM string.
    expect(label).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
  });

  it("returns the fallback when epoch is not a finite number", () => {
    expect(generatedLabel(NaN, "fallback text")).toBe("fallback text");
    expect(generatedLabel(Infinity, "fallback text")).toBe("fallback text");
  });

  it("returns the fallback when epoch is zero or negative", () => {
    expect(generatedLabel(0, "fallback")).toBe("fallback");
    expect(generatedLabel(-1, "fallback")).toBe("fallback");
  });

  it("pads single-digit months and days", () => {
    // Jan 5, 2026, 03:07 UTC → check for zero-padded segments.
    const ms = Date.UTC(2026, 0, 5, 3, 7, 0);
    const label = generatedLabel(ms, "fallback");
    // Extract the date portion — it should have padded month and day.
    // In the UTC timezone this would be "2026-01-05 03:07".
    // Other timezones shift the hour but keep the padded format.
    expect(label).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
  });
});

describe("splitReset", () => {
  it("splits a full 'resets in <duration>' string (GLM/Alibaba)", () => {
    expect(splitReset("resets in 2h 15m")).toEqual({ label: "resets in", time: "2h 15m" });
    expect(splitReset("resets in 3d 0h")).toEqual({ label: "resets in", time: "3d 0h" });
  });

  it("treats a bare duration as the time part (Claude path)", () => {
    expect(splitReset("45m")).toEqual({ label: "resets in", time: "45m" });
  });

  it("keeps a calendar date as the time part", () => {
    expect(splitReset("resets in 2026-06-24")).toEqual({
      label: "resets in",
      time: "2026-06-24",
    });
  });

  it("returns no label for placeholder/state values so they render plain", () => {
    expect(splitReset("—")).toEqual({ label: "", time: "—" });
    expect(splitReset("ready")).toEqual({ label: "", time: "ready" });
    expect(splitReset("resetting")).toEqual({ label: "", time: "resetting" });
    expect(splitReset("")).toEqual({ label: "", time: "" });
  });

  it("leaves a used/total count plain (Copilot meter is not a reset)", () => {
    // Copilot's only quota meter carries "used / total", not a countdown —
    // it must not be mislabeled "resets in" or colored as a time part.
    expect(splitReset("1.2K / 10K")).toEqual({ label: "", time: "1.2K / 10K" });
    expect(splitReset("0 / 300")).toEqual({ label: "", time: "0 / 300" });
  });

  it("trims surrounding whitespace", () => {
    expect(splitReset("  resets in 1h 30m  ")).toEqual({
      label: "resets in",
      time: "1h 30m",
    });
  });
});
