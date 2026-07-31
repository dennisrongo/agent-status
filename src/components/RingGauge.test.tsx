import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { RingGauge } from "./RingGauge";

describe("RingGauge", () => {
  it("renders the children at the center of the ring", () => {
    render(
      <RingGauge pct={42}>
        <div className="k-num">42%</div>
      </RingGauge>,
    );
    expect(screen.getByText("42%")).toBeInTheDocument();
  });

  it("exposes the percentage as the --pct custom property for the CSS arc", () => {
    const { container } = render(
      <RingGauge pct={42}>
        <span>42%</span>
      </RingGauge>,
    );
    const ring = container.querySelector(".ring") as HTMLElement;
    expect(ring.style.getPropertyValue("--pct")).toBe("42");
  });

  it("applies the status class to the root element", () => {
    const { container } = render(
      <RingGauge pct={91} status="danger">
        <span>91%</span>
      </RingGauge>,
    );
    expect(container.firstChild).toHaveClass("ring", "danger");
  });

  it("defaults to the ok status", () => {
    const { container } = render(
      <RingGauge pct={10}>
        <span>10%</span>
      </RingGauge>,
    );
    expect(container.firstChild).toHaveClass("ring", "ok");
  });

  it("clamps percentages above 100", () => {
    const { container } = render(
      <RingGauge pct={140}>
        <span>140%</span>
      </RingGauge>,
    );
    const ring = container.querySelector(".ring") as HTMLElement;
    expect(ring.style.getPropertyValue("--pct")).toBe("100");
  });

  it("clamps negative percentages to zero", () => {
    const { container } = render(
      <RingGauge pct={-5}>
        <span>-5%</span>
      </RingGauge>,
    );
    const ring = container.querySelector(".ring") as HTMLElement;
    expect(ring.style.getPropertyValue("--pct")).toBe("0");
  });

  it("labels the gauge with the rounded percentage for assistive tech", () => {
    render(
      <RingGauge pct={73.5}>
        <span>73.5%</span>
      </RingGauge>,
    );
    expect(screen.getByRole("img")).toHaveAccessibleName("74% used");
  });

  it("renders a track circle and an arc circle", () => {
    const { container } = render(
      <RingGauge pct={50}>
        <span>50%</span>
      </RingGauge>,
    );
    expect(container.querySelector(".ring-track")).toBeInTheDocument();
    expect(container.querySelector(".ring-arc")).toBeInTheDocument();
  });
});
