import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { WeekChart } from "./WeekChart";
import type { WeekDay } from "../types";

function makeWeek(): WeekDay[] {
  return [
    { day: "Mon", date: "2026-06-15", tokFmt: "1.2K", costFmt: "$0.50", barPct: 40 },
    { day: "Tue", date: "2026-06-16", tokFmt: "3.0K", costFmt: "$2.00", barPct: 100 },
    { day: "Wed", date: "2026-06-17", tokFmt: "500", costFmt: "$0.05", barPct: 15 },
    { day: "Thu", date: "2026-06-18", tokFmt: "0", costFmt: "$0.00", barPct: 0 },
    { day: "Fri", date: "2026-06-19", tokFmt: "2.0K", costFmt: "$1.00", barPct: 65 },
    { day: "Sat", date: "2026-06-20", tokFmt: "0", costFmt: "$0.00", barPct: 0 },
    { day: "Sun", date: "2026-06-21", tokFmt: "800", costFmt: "$0.10", barPct: 25 },
  ];
}

describe("WeekChart", () => {
  it("renders exactly 7 bar columns", () => {
    const { container } = render(<WeekChart week={makeWeek()} />);
    const cols = container.querySelectorAll(".bar-col");
    expect(cols).toHaveLength(7);
  });

  it("renders day abbreviations", () => {
    render(<WeekChart week={makeWeek()} />);
    expect(screen.getByText("Mon")).toBeInTheDocument();
    expect(screen.getByText("Tue")).toBeInTheDocument();
    expect(screen.getByText("Wed")).toBeInTheDocument();
    expect(screen.getByText("Thu")).toBeInTheDocument();
    expect(screen.getByText("Fri")).toBeInTheDocument();
    expect(screen.getByText("Sat")).toBeInTheDocument();
    expect(screen.getByText("Sun")).toBeInTheDocument();
  });

  it("renders the tooltip (tokens · cost) for each day", () => {
    render(<WeekChart week={makeWeek()} />);
    expect(screen.getByText("1.2K · $0.50")).toBeInTheDocument();
    expect(screen.getByText("3.0K · $2.00")).toBeInTheDocument();
    expect(screen.getByText("500 · $0.05")).toBeInTheDocument();
  });

  it("sets bar height from barPct", () => {
    const { container } = render(<WeekChart week={makeWeek()} />);
    const bars = container.querySelectorAll(".bar") as NodeListOf<HTMLElement>;
    // Tuesday has barPct=100 → height: 100%
    expect(bars[1].style.height).toBe("100%");
    // Thursday has barPct=0 → height: 0%
    expect(bars[3].style.height).toBe("0%");
    // Friday has barPct=65 → height: 65%
    expect(bars[4].style.height).toBe("65%");
  });

  it("renders empty week without crashing", () => {
    const { container } = render(<WeekChart week={[]} />);
    expect(container.querySelectorAll(".bar-col")).toHaveLength(0);
  });
});
