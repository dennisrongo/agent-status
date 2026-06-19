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
