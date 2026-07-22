import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act, waitFor } from "@testing-library/react";
import { useTauriCommand } from "./useTauriCommand";

// Mock the Tauri invoke function so tests don't need the real IPC bridge.
const mockInvoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

describe("useTauriCommand", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("starts in idle state (no data, not loading, no error)", () => {
    const { result } = renderHook(() => useTauriCommand<{ name: string }>("my_cmd"));
    expect(result.current.data).toBeNull();
    expect(result.current.isLoading).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it("sets isLoading=true during execute and resolves with data", async () => {
    mockInvoke.mockResolvedValue({ name: "result" });
    const { result } = renderHook(() =>
      useTauriCommand<{ name: string }>("my_cmd"),
    );

    let promise: Promise<unknown> | undefined;
    act(() => {
      promise = result.current.execute();
    });

    // During the async call, isLoading should be true.
    expect(result.current.isLoading).toBe(true);
    expect(result.current.error).toBeNull();

    await promise;

    // After the promise resolves, the state update flushes.
    await waitFor(() => {
      expect(result.current.data).toEqual({ name: "result" });
    });
    expect(result.current.isLoading).toBe(false);
    expect(result.current.error).toBeNull();
  });

  it("passes args to invoke", async () => {
    mockInvoke.mockResolvedValue("ok");
    const { result } = renderHook(() => useTauriCommand<string>("do_thing"));

    await act(async () => {
      await result.current.execute({ id: 42, label: "hello" });
    });

    expect(mockInvoke).toHaveBeenCalledWith("do_thing", {
      id: 42,
      label: "hello",
    });
  });

  it("catches errors and sets the error string", async () => {
    mockInvoke.mockRejectedValue(new Error("network failed"));
    const { result } = renderHook(() => useTauriCommand<string>("fetch"));

    await act(async () => {
      await result.current.execute();
    });

    expect(result.current.data).toBeNull();
    expect(result.current.isLoading).toBe(false);
    expect(result.current.error).toBe("network failed");
  });

  it("handles non-Error rejections by stringifying them", async () => {
    mockInvoke.mockRejectedValue("plain string error");
    const { result } = renderHook(() => useTauriCommand<string>("fetch"));

    await act(async () => {
      await result.current.execute();
    });

    expect(result.current.error).toBe("plain string error");
  });

  it("clears error on a new execute call after a failure", async () => {
    mockInvoke.mockRejectedValueOnce(new Error("fail"));
    mockInvoke.mockResolvedValueOnce("ok");
    const { result } = renderHook(() => useTauriCommand<string>("fetch"));

    await act(async () => {
      await result.current.execute();
    });
    expect(result.current.error).toBe("fail");

    await act(async () => {
      await result.current.execute();
    });
    expect(result.current.error).toBeNull();
    expect(result.current.data).toBe("ok");
  });
});
