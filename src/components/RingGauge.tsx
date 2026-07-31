import type { CSSProperties, ReactNode } from "react";

// 100×100 viewBox with r=45 keeps the 7-unit stroke inside the box, so the
// SVG can size itself with plain CSS (width/height 100%) and no overflow
// clipping is needed. pathLength=100 normalizes dash math to percentage
// space, so the CSS drives the arc with plain 0–100 values.
const R = 45;

/**
 * A circular gauge drawn around the headline number of a KPI tile. The arc
 * sweeps clockwise from 12 o'clock to `pct` of the circle; its color comes
 * from the `status` class (ok/warn/danger), matching the meter bars. The arc
 * length is a CSS custom property (`--pct`): CSS animates the draw-in each
 * time the provider panel mounts (tab focus / auto-rotate) and transitions
 * quiet re-settles when usage refreshes while the panel is on screen.
 */
export function RingGauge({
  pct,
  status = "ok",
  children,
}: {
  pct: number;
  status?: "ok" | "warn" | "danger";
  children: ReactNode;
}) {
  const clamped = Math.max(0, Math.min(100, pct));
  return (
    <div
      className={`ring ${status}`}
      role="img"
      aria-label={`${Math.round(clamped)}% used`}
      style={{ "--pct": clamped } as CSSProperties}
    >
      <svg viewBox="0 0 100 100" aria-hidden="true">
        <circle className="ring-track" cx="50" cy="50" r={R} pathLength={100} />
        <circle
          className="ring-arc"
          cx="50" cy="50" r={R} pathLength={100}
          transform="rotate(-90 50 50)"
        />
      </svg>
      <div className="ring-num">{children}</div>
    </div>
  );
}
