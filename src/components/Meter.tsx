import type { Bucket } from "../types";

export function Meter({ bucket }: { bucket: Bucket }) {
  return (
    <div className={`meter ${bucket.status}`}>
      <div className="meter-top">
        <span className="ml">
          {bucket.name}
          <span className="ms">{bucket.sub}</span>
        </span>
        <span className="reset">
          resets in <b>{bucket.reset}</b>
        </span>
      </div>
      <div className="track">
        <div
          className={`fill ${bucket.status}`}
          style={{ width: `${bucket.usedPct}%` }}
        />
      </div>
      <div className="meter-foot">
        {bucket.live ? (
          <span className="mu live-tag">● live · Claude</span>
        ) : (
          <span className="mu">
            <b>{bucket.usedFmt}</b> / {bucket.limitFmt}
          </span>
        )}
        <span className="ml2">
          <b>{bucket.usedPct}%</b> used
        </span>
      </div>
    </div>
  );
}
