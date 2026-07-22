import { describe, it, expect, afterEach } from "vitest";
import { isTauriReady } from "./tauriReady";

describe("isTauriReady", () => {
  const original = window;

  afterEach(() => {
    // Restore the real window so other tests aren't affected.
    Object.defineProperty(globalThis, "window", {
      value: original,
      writable: true,
      configurable: true,
    });
  });

  it("returns false when __TAURI_INTERNALS__ is absent", () => {
    Object.defineProperty(globalThis, "window", {
      value: {},
      writable: true,
      configurable: true,
    });
    expect(isTauriReady()).toBe(false);
  });

  it("returns true when __TAURI_INTERNALS__ is present", () => {
    Object.defineProperty(globalThis, "window", {
      value: { __TAURI_INTERNALS__: {} },
      writable: true,
      configurable: true,
    });
    expect(isTauriReady()).toBe(true);
  });
});
