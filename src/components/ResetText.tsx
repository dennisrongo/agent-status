import { splitReset } from "../format";

/**
 * Renders a reset-countdown line with the time part highlighted so it stands
 * out from the faint "resets" label. Shared by the KPI tiles (Claude buckets
 * and the GLM/Alibaba/Copilot quota meters) and the hover popover so every
 * provider renders the countdown the same way.
 *
 * Pass either a full "resets 2h 15m" string or a bare duration ("2h 15m").
 * State values ("ready", "resetting", "—", empty) render plain, uncolored.
 */
export function ResetText({ value }: { value: string }) {
  const { label, time } = splitReset(value);
  if (!label) return <>{time}</>;
  return (
    <>
      {label} <span className="k-reset-time">{time}</span>
    </>
  );
}
