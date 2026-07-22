import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import type { UsageSnapshot, SettingsView } from "../types";

// ── Mocks ──

// Capture the event listener so we can emit synthetic "usage-updated" events.
let eventCallback: ((e: { payload: UsageSnapshot }) => void) | null = null;

const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((_event: string, cb: (e: { payload: UsageSnapshot }) => void) => {
    eventCallback = cb;
    return Promise.resolve(() => {});
  }),
}));

vi.mock("../tauriReady", () => ({
  isTauriReady: () => true,
}));

// ── Helpers ──

const mockSettings: SettingsView = {
  plan: "max5x",
  refreshSecs: 30,
  glmEndpoint: "https://api.z.ai/api/monitor/usage/quota/limit",
  glmKeySet: false,
  anthropicKeySet: false,
  copilotConnected: false,
  liveClaude: false,
  launchOnStartup: true,
  minimalView: false,
  tooltipProvider: "claude",
  windowMode: "dock",
  hiddenProviders: [],
};

function makeSnapshot(genMs: number): UsageSnapshot {
  return {
    meta: {
      generated: "2026-06-17 12:00 UTC",
      generatedMs: genMs,
      windowFirst: "2026-06-10",
      windowLast: "2026-06-17",
      filesScanned: 1,
    },
    limits: {
      planLabel: "Max 5×",
      estimateNote: "test note",
      buckets: [],
    },
    kpi: {
      sessionTokens: "300",
      sessionCost: "$0.01",
      weekTokens: "3K",
      weekCost: "$0.10",
      totalTokens: "33K",
      totalCost: "$1.00",
    },
    week: [],
    models: [],
    sessions: [],
    providers: [],
    glm: { sessions: 0, activeDays: 0, last: "—", note: "" },
  };
}

// ── Tests ──

describe("useUsage — out-of-order snapshot guard", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    eventCallback = null;
    // Default: get_settings returns settings, get_usage returns a snapshot.
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") return Promise.resolve(mockSettings);
      if (cmd === "get_usage") return Promise.resolve(makeSnapshot(1000));
      return Promise.resolve(null);
    });
  });

  it("loads the initial snapshot from get_usage on mount", async () => {
    const { useUsage } = await import("./useUsage");
    const { result } = renderHook(() => useUsage());

    await waitFor(() => {
      expect(result.current.snapshot).not.toBeNull();
    });
    expect(result.current.snapshot?.meta.generatedMs).toBe(1000);
  });

  it("drops an event with an older generatedMs", async () => {
    const { useUsage } = await import("./useUsage");
    const { result } = renderHook(() => useUsage());

    // Wait for initial load.
    await waitFor(() => {
      expect(result.current.snapshot?.meta.generatedMs).toBe(1000);
    });

    // Emit a snapshot with an OLDER timestamp → must be dropped.
    act(() => {
      eventCallback?.({ payload: makeSnapshot(500) });
    });

    expect(result.current.snapshot?.meta.generatedMs).toBe(1000);
  });

  it("accepts an event with a newer generatedMs", async () => {
    const { useUsage } = await import("./useUsage");
    const { result } = renderHook(() => useUsage());

    await waitFor(() => {
      expect(result.current.snapshot?.meta.generatedMs).toBe(1000);
    });

    // Emit a snapshot with a NEWER timestamp → must replace the displayed one.
    act(() => {
      eventCallback?.({ payload: makeSnapshot(2000) });
    });

    expect(result.current.snapshot?.meta.generatedMs).toBe(2000);
  });

  it("accepts equal generatedMs (not strictly greater)", async () => {
    const { useUsage } = await import("./useUsage");
    const { result } = renderHook(() => useUsage());

    await waitFor(() => {
      expect(result.current.snapshot?.meta.generatedMs).toBe(1000);
    });

    // Same timestamp → accepted (the guard is `gen < lastGenMs`, not `<=`).
    act(() => {
      eventCallback?.({ payload: makeSnapshot(1000) });
    });

    expect(result.current.snapshot?.meta.generatedMs).toBe(1000);
  });
});

describe("useUsage — settings load", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    eventCallback = null;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") return Promise.resolve(mockSettings);
      if (cmd === "get_usage") return Promise.resolve(makeSnapshot(1000));
      return Promise.resolve(null);
    });
  });

  it("loads settings on mount", async () => {
    const { useUsage } = await import("./useUsage");
    const { result } = renderHook(() => useUsage());

    await waitFor(() => {
      expect(result.current.settings).not.toBeNull();
    });
    expect(result.current.settings?.plan).toBe("max5x");
    expect(result.current.settings?.refreshSecs).toBe(30);
  });

  it("refresh calls get_usage and updates the snapshot", async () => {
    // After initial load, make get_usage return a newer snapshot.
    const { useUsage } = await import("./useUsage");
    const { result } = renderHook(() => useUsage());

    await waitFor(() => {
      expect(result.current.snapshot?.meta.generatedMs).toBe(1000);
    });

    // Next get_usage call returns a newer snapshot.
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") return Promise.resolve(mockSettings);
      if (cmd === "get_usage") return Promise.resolve(makeSnapshot(3000));
      return Promise.resolve(null);
    });

    await act(async () => {
      await result.current.refresh();
    });

    expect(result.current.snapshot?.meta.generatedMs).toBe(3000);
  });
});
