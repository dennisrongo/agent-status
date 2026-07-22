import { describe, it, expect, vi, beforeEach } from "vitest";
import { isWindows, fitWindowHeight } from "./platform";

// Mock the Tauri IPC bridge.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

// Mock the window module so we can spy on setSize.
const mockSetSize = vi.fn().mockResolvedValue(undefined);
vi.mock("@tauri-apps/api/window", () => ({
  LogicalSize: class {
    constructor(
      public width: number,
      public height: number,
    ) {}
  },
  Window: class {
    label = "main";
    setSize = mockSetSize;
  },
}));

describe("isWindows", () => {
  it("is a boolean derived from navigator.userAgent", () => {
    expect(typeof isWindows).toBe("boolean");
  });
});

describe("fitWindowHeight", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("calls setSize directly on non-Windows or floating mode", async () => {
    const { Window } = await import("@tauri-apps/api/window");
    const win = new Window("main");
    // isWindows is a const — if running on Windows it's true, else false.
    // On a non-Windows host (or floating=true), it should call setSize.
    await fitWindowHeight(win as any, 440, 660, true);
    expect(mockSetSize).toHaveBeenCalledOnce();
  });

  it("invokes the fit_tray_window command on Windows non-floating", async () => {
    const { invoke } = await import("@tauri-apps/api/core");
    const { Window } = await import("@tauri-apps/api/window");
    const win = new Window("main");

    // On Windows, isWindows is true and floating=false → should call invoke.
    // We can't force isWindows, but if we're on Windows the branch is taken.
    // On non-Windows hosts, isWindows is false so setSize is used instead.
    // Either way, one of the two paths fires.
    await fitWindowHeight(win as any, 440, 500, false);

    if (isWindows) {
      expect(invoke).toHaveBeenCalledWith("fit_tray_window", {
        label: "main",
        width: 440,
        height: 500,
      });
      expect(mockSetSize).not.toHaveBeenCalled();
    } else {
      expect(mockSetSize).toHaveBeenCalledOnce();
      expect(invoke).not.toHaveBeenCalled();
    }
  });
});
