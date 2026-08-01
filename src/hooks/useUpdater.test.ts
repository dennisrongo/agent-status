import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

// Mock the Tauri invoke function so tests don't need the real IPC bridge.
const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

// Mock the updater plugin's `check`. Each test seeds the resolved value.
const mockCheck = vi.fn();
vi.mock("@tauri-apps/plugin-updater", () => ({
  check: (...args: unknown[]) => mockCheck(...args),
}));

// Import after mocks are registered.
import { useUpdater } from "./useUpdater";

describe("useUpdater", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("auto mode: toggles the tray badge via set_update_available on a definitive check", async () => {
    // An update is available at the endpoint.
    mockCheck.mockResolvedValue({ version: "9.9.9", downloadAndInstall: vi.fn() });

    const { result } = renderHook(() => useUpdater({ auto: true }));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(result.current.phase).toBe("available");
    expect(result.current.version).toBe("9.9.9");
    // The tray badge command should reflect availability.
    expect(mockInvoke).toHaveBeenCalledWith("set_update_available", {
      available: true,
    });
  });

  it("auto mode: clears the badge when up to date", async () => {
    mockCheck.mockResolvedValue(null);

    renderHook(() => useUpdater({ auto: true }));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(mockInvoke).toHaveBeenCalledWith("set_update_available", {
      available: false,
    });
  });

  it("manual mode: does not auto-check or touch the tray badge", async () => {
    mockCheck.mockResolvedValue(null);

    const { result } = renderHook(() => useUpdater({ auto: false }));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    // No automatic check, no badge command — manual callers drive check() themselves.
    expect(mockCheck).not.toHaveBeenCalled();
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "set_update_available",
      expect.anything(),
    );
    expect(result.current.phase).toBe("idle");
  });

  it("auto mode: swallows check failures silently (no badge change, no throw)", async () => {
    mockCheck.mockRejectedValue(new Error("offline"));

    const { result } = renderHook(() => useUpdater({ auto: true }));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    // A transient failure must not flip the badge either way.
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "set_update_available",
      expect.anything(),
    );
    // Stays in its initial idle-ish state (the catch is silent in auto mode).
    expect(result.current.phase).toBe("idle");
  });

  it("auto mode: re-checks on the interval and re-notifies the tray", async () => {
    mockCheck.mockResolvedValue(null);
    renderHook(() => useUpdater({ auto: true }));

    // Initial mount check.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(mockCheck).toHaveBeenCalledTimes(1);

    // Advance past one interval (4h) — the next check fires.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(4 * 60 * 60 * 1000);
    });
    expect(mockCheck).toHaveBeenCalledTimes(2);
  });
});
