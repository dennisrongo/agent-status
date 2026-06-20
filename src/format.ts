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
