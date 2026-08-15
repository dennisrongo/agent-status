// Shared formatting helpers used by both the main window and the hover popover.

/**
 * Short label for a usage bucket, matching the Overview KPI tiles:
 * "Session", "Week" for the all-models window, and "<model> wk" for a
 * model-scoped weekly window (e.g. "Opus wk").
 */
export function tileLabel(name: string): string {
  if (name.startsWith("Session")) return "Session";
  if (name.includes("all models")) return "Week";
  const scope = name.split("·").pop()?.trim();
  return scope ? `${scope} wk` : name;
}

/**
 * Format the snapshot's generation instant in the machine's local timezone,
 * e.g. "2026-06-20 14:32". The backend stamps `generated` as a UTC string, but
 * `generatedMs` is an absolute epoch we can render in local time here. Falls
 * back to the raw UTC string when the epoch is missing/invalid.
 */
export function generatedLabel(generatedMs: number, fallback: string): string {
  if (!Number.isFinite(generatedMs) || generatedMs <= 0) return fallback;
  const d = new Date(generatedMs);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(
    d.getHours(),
  )}:${pad(d.getMinutes())}`;
}

/**
 * Split a reset-countdown string into the "resets in" label and the time
 * part that follows it, so the time can be colored separately. Recognizes a
 * full "resets in 2h 15m" string (GLM/Alibaba), a composite
 * "<counts> · resets in 3d 0h" (GLM tool quota), the bare "resets <date>"
 * form, or a bare duration like "2h 15m" (Claude). Composites keep their
 * counts in the plain color — only the time part is highlighted. Anything
 * else — used/total counts ("1.2K / 10K"), placeholders ("—"), or state
 * words ("ready") — returns no label so the caller renders it plain and
 * uncolored.
 */
export function splitReset(value: string): { label: string; time: string } {
  const v = value.trim();
  // The marker either leads the string or follows the " · " that joins it to
  // counts; the optional "in" absorbs the bare "resets <date>" form.
  const m = v.match(/^(.*?· )?resets (?:in )?(.+)$/);
  // A bare duration (e.g. "2h 15m", "3d 0h", "45m") from the Claude path.
  if (!m) {
    return /^\d+[dhm]/.test(v)
      ? { label: "resets in", time: v }
      : { label: "", time: v };
  }
  return { label: `${m[1] ?? ""}resets in`, time: m[2] };
}
