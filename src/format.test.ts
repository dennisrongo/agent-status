import { describe, it, expect } from "vitest";
import { tileLabel, generatedLabel } from "./format";

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
