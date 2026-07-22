import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { Meter } from "./Meter";
import type { Bucket } from "../types";

function makeBucket(overrides: Partial<Bucket> = {}): Bucket {
  return {
    name: "Session",
    sub: "5-hour window",
    usedFmt: "12K",
    usedPct: 40,
    leftPct: 60,
    leftFmt: "18K",
    limitFmt: "30K",
    reset: "2h 15m",
    status: "ok",
    statusLabel: "Healthy",
    live: false,
    ...overrides,
  };
}

describe("Meter", () => {
  it("renders the bucket name and sub-label", () => {
    render(<Meter bucket={makeBucket()} />);
    expect(screen.getByText("Session")).toBeInTheDocument();
    // The sub-label "5-hour window" is rendered as a child of the name span.
    expect(screen.getByText("5-hour window")).toBeInTheDocument();
  });

  it("renders the reset countdown", () => {
    render(<Meter bucket={makeBucket({ reset: "4h 30m" })} />);
    expect(screen.getByText("4h 30m")).toBeInTheDocument();
  });

  it("applies the status class to the root element", () => {
    const { container } = render(<Meter bucket={makeBucket({ status: "danger" })} />);
    expect(container.firstChild).toHaveClass("meter", "danger");
  });

  it("applies the status class to the fill bar", () => {
    const { container } = render(<Meter bucket={makeBucket({ status: "warn" })} />);
    const fill = container.querySelector(".fill");
    expect(fill).toHaveClass("warn");
  });

  it("sets the fill width to usedPct percent", () => {
    const { container } = render(<Meter bucket={makeBucket({ usedPct: 73.5 })} />);
    const fill = container.querySelector(".fill") as HTMLElement;
    expect(fill.style.width).toBe("73.5%");
  });

  it("shows live tag when bucket.live is true", () => {
    render(<Meter bucket={makeBucket({ live: true })} />);
    expect(screen.getByText(/live/i)).toBeInTheDocument();
    // The used/limit line is not shown when live.
    expect(screen.queryByText(/30K/)).not.toBeInTheDocument();
  });

  it("shows used/limit when bucket.live is false", () => {
    const { container } = render(
      <Meter bucket={makeBucket({ usedFmt: "12K", limitFmt: "30K", live: false })} />,
    );
    // The used/limit are in the same .mu span as "12K / 30K".
    const mu = container.querySelector(".mu");
    expect(mu?.textContent).toContain("12K");
    expect(mu?.textContent).toContain("30K");
  });

  it("renders the used percentage value", () => {
    render(<Meter bucket={makeBucket({ usedPct: 55 })} />);
    expect(screen.getByText("55%")).toBeInTheDocument();
  });
});
