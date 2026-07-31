import type { CSSProperties } from "react";
import type { WeekDay } from "../types";

export function WeekChart({ week }: { week: WeekDay[] }) {
  return (
    <div className="chart">
      <div className="bars">
        {week.map((d, i) => (
          <div className="bar-col" key={d.date}>
            <span className="tip">
              {d.tokFmt} · {d.costFmt}
            </span>
            <div className="bar-wrap">
              <div
                className="bar"
                style={{ height: `${d.barPct}%`, "--i": i } as CSSProperties}
              />
            </div>
            <span className="day">{d.day}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
