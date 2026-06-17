import type { VendorStatus } from "../types";

export function VendorCard({ name, status }: { name: string; status: VendorStatus }) {
  const state = !status.configured ? "idle" : status.ok ? "ok" : "err";
  return (
    <div className="prov-row">
      <span className={`stat ${state}`} />
      <div>
        <div className="pname">{name}</div>
        <div className="pmeta">
          {!status.configured
            ? "no API key set"
            : status.ok
              ? status.secondary
              : status.error || "fetch failed"}
        </div>
      </div>
      <span className="spacer" />
      <div>
        <div className="pnum">{status.primary}</div>
        {status.detail.length > 0 && (
          <div className="pcost">{status.detail[status.detail.length - 1].label}</div>
        )}
      </div>
    </div>
  );
}
