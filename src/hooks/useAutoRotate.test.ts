import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useAutoRotate } from "./useAutoRotate";

type ProviderTab = "claude" | "glm" | "copilot" | "alibaba" | "kimi" | "grok" | "codex";

describe("useAutoRotate", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("advances to the next provider on each tick", () => {
    let current: ProviderTab = "claude";
    const setProvider = vi.fn((updater: (prev: ProviderTab) => ProviderTab) => {
      current = updater(current);
    });
    const tabs: ProviderTab[] = ["claude", "glm", "copilot"];

    renderHook(() => useAutoRotate(true, 10_000, tabs, setProvider));

    act(() => { vi.advanceTimersByTime(10_000); });
    expect(current).toBe("glm");

    act(() => { vi.advanceTimersByTime(10_000); });
    expect(current).toBe("copilot");

    act(() => { vi.advanceTimersByTime(10_000); });
    expect(current).toBe("claude");
  });

  it("does not advance when disabled", () => {
    const setProvider = vi.fn();
    const tabs: ProviderTab[] = ["claude", "glm"];

    renderHook(() => useAutoRotate(false, 10_000, tabs, setProvider));

    act(() => { vi.advanceTimersByTime(30_000); });
    expect(setProvider).not.toHaveBeenCalled();
  });

  it("does not advance with a single tab", () => {
    const setProvider = vi.fn();
    const tabs: ProviderTab[] = ["claude"];

    renderHook(() => useAutoRotate(true, 10_000, tabs, setProvider));

    act(() => { vi.advanceTimersByTime(30_000); });
    expect(setProvider).not.toHaveBeenCalled();
  });

  it("stops when disabled after being enabled", () => {
    let current: ProviderTab = "claude";
    const setProvider = vi.fn((updater: (prev: ProviderTab) => ProviderTab) => {
      current = updater(current);
    });
    const tabs: ProviderTab[] = ["claude", "glm"];

    const { rerender } = renderHook(
      ({ enabled }) => useAutoRotate(enabled, 10_000, tabs, setProvider),
      { initialProps: { enabled: true } },
    );

    act(() => { vi.advanceTimersByTime(10_000); });
    expect(current).toBe("glm");

    rerender({ enabled: false });
    act(() => { vi.advanceTimersByTime(30_000); });
    expect(current).toBe("glm");
  });

  it("recovers when the current provider is not in the tab list", () => {
    let current: ProviderTab = "alibaba";
    const setProvider = vi.fn((updater: (prev: ProviderTab) => ProviderTab) => {
      current = updater(current);
    });
    const tabs: ProviderTab[] = ["claude", "glm"];

    renderHook(() => useAutoRotate(true, 10_000, tabs, setProvider));

    act(() => { vi.advanceTimersByTime(10_000); });
    expect(current).toBe("glm");
  });
});
