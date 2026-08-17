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
  provider: "claude" | "glm" | "copilot" | "alibaba" | "kimi" | "grok" | "codex";
  /** Pre-rendered by Rust — "1.2M", or "—" when the provider records no count. */
  tokensText: string;
  /** Pre-rendered by Rust — dollars, premium requests, or "—". */
  costText: string;
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
  /** Percent used (0–100) when this row is a quota meter; drives the bar. */
  pct?: number;
  status?: "ok" | "warn" | "danger";
}

export interface VendorStatus {
  configured: boolean;
  ok: boolean;
  error: string | null;
  primary: string;
  secondary: string;
  detail: VendorKeyVal[];
  /** The failure is a stale/missing login specifically (e.g. the Bailian
   * console session expired even though `bl auth status` still reports
   * `authenticated`). Discovered only by attempting a usage call, so this is
   * the authoritative signal — both Overview and Settings read it off the
   * snapshot. A transient/network `ok:false` leaves this `false`. */
  authExpired: boolean;
}

export interface VendorReport {
  glm: VendorStatus;
  anthropic: VendorStatus;
  copilot: VendorStatus;
  alibaba: VendorStatus;
  kimi: VendorStatus;
  grok: VendorStatus;
  codex: VendorStatus;
}

/** z.ai "Last 7 days" view, fetched from the monitor `model-usage` endpoint.
 *  `days` reuses the Claude week-chart shape — `costFmt` carries the day's call
 *  count ("1.2K calls") because z.ai reports no cost. */
export interface GlmWeek {
  days: WeekDay[];
  models: VendorKeyVal[];
  totalTokens: string;
  totalCalls: string;
}

/** Local Grok CLI token totals for the xAI Overview. SuperGrok has no
 *  public % ceiling, so this is the real usage the tab can show. */
export interface GrokWeek {
  days: WeekDay[];
  models: ModelRow[];
  /** Tokens in the last 5 hours — local spend, not a vendor quota. */
  sessionTokens: string;
  weekTokens: string;
  totalTokens: string;
  sessions: number;
  last: string;
}

/** Local Codex CLI token totals for the OpenAI Overview. Plus/Pro has no
 *  public dollar rate, so cost is always an em dash. */
export interface CodexWeek {
  days: WeekDay[];
  models: ModelRow[];
  sessionTokens: string;
  weekTokens: string;
  totalTokens: string;
  sessions: number;
  last: string;
  /** Last 5-hour / weekly % captured from a session `rate_limits` snapshot. */
  windows?: VendorKeyVal[];
}

export interface Detection {
  claude: boolean;
  glm: boolean;
  copilot: boolean;
  alibaba: boolean;
  kimi: boolean;
  grok: boolean;
  codex: boolean;
  /** A Claude Code OAuth login is present on this machine (independent of the
   * live toggle). Drives the connect/disconnect control. */
  claudeSignedIn: boolean;
  /** That login is present but past its expiry (after any auto-refresh) — Settings
   * shows a reconnect affordance instead of a misleading "connected" one. */
  claudeExpired: boolean;
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
  /** Present once a z.ai key is set and the 7-day usage fetch has succeeded. */
  glmWeek?: GlmWeek;
  /** Present when local Grok CLI session logs were found. */
  grokWeek?: GrokWeek;
  /** Present when local Codex CLI session logs were found. */
  codexWeek?: CodexWeek;
  detection?: Detection;
}

export interface CopilotDeviceCode {
  userCode: string;
  verificationUri: string;
  interval: number;
}

export interface ClaudeLoginInfo {
  authorizeUrl: string;
}

export interface BailianCliStatus {
  installed: boolean;
  authenticated: boolean;
  authHint: string | null;
  /** OpenAPI AK/SK credentials are configured — the CLI can auto-refresh the
   * console session token, so the user won't need to re-login manually. */
  hasOpenApi: boolean;
}

export interface KimiCliStatus {
  installed: boolean;
  authenticated: boolean;
}

export interface GrokCliStatus {
  installed: boolean;
  authenticated: boolean;
}

export interface CodexCliStatus {
  installed: boolean;
  authenticated: boolean;
}

export interface CodexLoginInfo {
  authorizeUrl: string;
}

/** Device-flow details pushed via the `kimi-login-device` event while a
 * `kimi login` is in flight — the Settings UI shows the code so the user can
 * complete the login even if the browser didn't open. */
export interface KimiDeviceLogin {
  verificationUrl: string;
  userCode: string;
}

export type PlanKey = "pro" | "max5x" | "max20x" | "custom";

export type TooltipProvider = "claude" | "glm" | "copilot" | "alibaba" | "kimi" | "grok" | "codex";

export type WindowMode = "dock" | "float";

export interface SettingsView {
  plan: PlanKey;
  refreshSecs: number;
  glmEndpoint: string;
  glmKeySet: boolean;
  anthropicKeySet: boolean;
  copilotConnected: boolean;
  liveClaude: boolean;
  launchOnStartup: boolean;
  minimalView: boolean;
  tooltipProvider: TooltipProvider;
  windowMode: WindowMode;
  hiddenProviders: string[];
  autoRotate: boolean;
  rotateSecs: number;
  mcpEnabled: boolean;
}

/** One AI agent the agent-status MCP server can be registered with. */
export interface McpAgent {
  id: string;
  name: string;
  detected: boolean;
  registered: boolean;
  configPath: string;
  commandPath: string | null;
}
