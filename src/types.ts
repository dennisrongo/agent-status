// Mirrors the Rust `UsageSnapshot` (serialized camelCase).

export interface Meta {
  generated: string;
  generatedMs: number;
  windowFirst: string;
  windowLast: string;
  filesScanned: number;
}

export interface Bucket {
  name: string;
  sub: string;
  usedFmt: string;
  usedPct: number;
  leftPct: number;
  leftFmt: string;
  limitFmt: string;
  reset: string;
  status: "ok" | "warn" | "danger";
  statusLabel: string;
  live?: boolean;
}

export interface Limits {
  planLabel: string;
  estimateNote: string;
  buckets: Bucket[];
  /** Meters are real live data from Claude's usage API. */
  live?: boolean;
  /** Live data is the chosen source but not available yet — show a loading
   * state instead of the (wrong-scale) local estimate. */
  pending?: boolean;
  /** A Claude Code login exists but expired (HTTP 401) — show an actionable
   * "sign in again" state rather than an indistinguishable loading spinner. */
  needsReauth?: boolean;
  /** Live mode is on but no Claude Code login is present — bars are a local
   * estimate. Show a "not signed in" hint instead of silently relabeling. */
  signedOut?: boolean;
}

export interface Kpi {
  sessionTokens: string;
  sessionCost: string;
  weekTokens: string;
  weekCost: string;
  totalTokens: string;
  totalCost: string;
}

export interface WeekDay {
  day: string;
  date: string;
  tokFmt: string;
  costFmt: string;
  barPct: number;
}

export interface ModelRow {
  name: string;
  key: string;
  tokens: string;
  cost: string;
  pct: number;
}

export interface SessionRow {
  id: string;
  project: string;
  model: string;
  tokens: number;
  cost: number;
  when: string;
}

export interface Provider {
  name: string;
  status: string;
  tokens: string;
  cost: string;
  sessions: number;
}

export interface Glm {
  sessions: number;
  activeDays: number;
  last: string;
  note: string;
}

export interface VendorKeyVal {
  label: string;
  value: string;
}

export interface VendorStatus {
  configured: boolean;
  ok: boolean;
  error: string | null;
  primary: string;
  secondary: string;
  detail: VendorKeyVal[];
}

export interface VendorReport {
  glm: VendorStatus;
  anthropic: VendorStatus;
}

export interface Detection {
  claude: boolean;
  glm: boolean;
}

export interface UsageSnapshot {
  meta: Meta;
  limits: Limits;
  kpi: Kpi;
  week: WeekDay[];
  models: ModelRow[];
  sessions: SessionRow[];
  providers: Provider[];
  glm: Glm;
  vendor?: VendorReport;
  detection?: Detection;
}

export type PlanKey = "pro" | "max5x" | "max20x" | "custom";

export type TooltipProvider = "claude" | "glm";

export interface SettingsView {
  plan: PlanKey;
  refreshSecs: number;
  glmEndpoint: string;
  glmKeySet: boolean;
  anthropicKeySet: boolean;
  liveClaude: boolean;
  launchOnStartup: boolean;
  minimalView: boolean;
  tooltipProvider: TooltipProvider;
}
