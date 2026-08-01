import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";

// Mock useUpdater so we can control the phase without real Tauri IPC.
const mockState = { phase: "idle" as string, version: null as string | null, error: null as string | null, check: vi.fn(), install: vi.fn() };
vi.mock("../hooks/useUpdater", () => ({
  useUpdater: () => mockState,
}));

import { UpdateBanner } from "./UpdateBanner";

describe("UpdateBanner", () => {
  it("is hidden when phase is idle", () => {
    mockState.phase = "idle";
    const { container } = render(<UpdateBanner />);
    expect(container.innerHTML).toBe("");
  });

  it("is hidden when phase is checking", () => {
    mockState.phase = "checking";
    const { container } = render(<UpdateBanner />);
    expect(container.innerHTML).toBe("");
  });

  it("is hidden when phase is uptodate", () => {
    mockState.phase = "uptodate";
    const { container } = render(<UpdateBanner />);
    expect(container.innerHTML).toBe("");
  });

  it("shows 'Update available' when phase is available", () => {
    mockState.phase = "available";
    mockState.version = "1.2.3";
    render(<UpdateBanner />);
    expect(screen.getByText(/Update available/)).toBeInTheDocument();
    expect(screen.getByText(/v1\.2\.3/)).toBeInTheDocument();
  });

  it("shows error message when phase is error", () => {
    mockState.phase = "error";
    mockState.error = "network timeout";
    render(<UpdateBanner />);
    expect(screen.getByText(/Update failed/)).toBeInTheDocument();
    expect(screen.getByText(/network timeout/)).toBeInTheDocument();
  });
});
