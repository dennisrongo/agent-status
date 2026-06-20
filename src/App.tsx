import { useLayoutEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { About } from "./components/About";
import { Meter } from "./components/Meter";
import { Settings } from "./components/Settings";
import { UpdateBanner } from "./components/UpdateBanner";
import { WeekChart } from "./components/WeekChart";
import { useUsage } from "./hooks/useUsage";
import { fitWindowHeight, isWindows } from "./platform";
import { isTauriReady } from "./tauriReady";
import { tileLabel } from "./format";
import type { Glm, PlanKey, VendorKeyVal, VendorStatus } from "./types";

type Tab = "overview" | "sessions" | "providers" | "settings" | "about";

// Full window height from tauri.conf.json. Minimal view shrinks below this to
// fit the headline stats; everything else uses the full height.
const FULL_HEIGHT = 660;
const WINDOW_WIDTH = 440;

const PLANS: { key: PlanKey; label: string }[] = [
  { key: "pro", label: "Pro" },
  { key: "max5x", label: "Max 5×" },
  { key: "max20x", label: "Max 20×" },
];

export default function App() {
  const {
    snapshot,
    settings,
    setPlan,
    setApiKey,
    clearApiKey,
    setGlmEndpoint,
    setRefreshSecs,
    setLiveClaude,
    setLaunchOnStartup,
    setMinimalView,
    setTooltipProvider,
    reloadSettings,
    connectCopilotStart,
    copilotPoll,
    copilotCancel,
    disconnectCopilot,
    refresh,
    reconnectClaude,
    reconnecting,
    reconnectError,
    isLoading,
    error,
    keyError,
  } = useUsage();
  const [tab, setTab] = useState<Tab>("overview");
  const [provider, setProvider] = useState<"claude" | "glm" | "copilot">("claude");
  const plan: PlanKey = settings?.plan ?? "max5x";
  // Minimal view only trims the Overview; other tabs always show full content.
  const minimal = (settings?.minimalView ?? false) && tab === "overview";

  // Fit the window to its content in minimal view (no scrollbar, no dead
  // space); restore the full height otherwise. macOS is anchored top-under-tray
  // so resizing grows/shrinks downward; on Windows fitWindowHeight keeps the
  // bottom edge pinned above the taskbar instead. useLayoutEffect (not
  // useEffect) so the resize is dispatched in the same frame the view changes —
  // switching feels instant instead of lagging a paint behind.
  useLayoutEffect(() => {
    if (!isTauriReady()) return;
    const win = getCurrentWindow();
    const root = document.querySelector<HTMLElement>(".widget");
    const body = document.querySelector<HTMLElement>(".body");
    const panel = body?.firstElementChild as HTMLElement | null;
    if (!minimal || !root || !body || !panel) {
      fitWindowHeight(win, WINDOW_WIDTH, FULL_HEIGHT).catch(() => {});
      return;
    }
    // Fit to the panel's own height. NB: not body.scrollHeight — that clamps to
    // the viewport when content underflows, so it can never shrink the window
    // (and the buffer would make it creep upward each refresh). The panel's
    // height is set by its content at the fixed window width, so it's stable —
    // which is also why the fixed BREATHING_ROOM below can't accumulate.
    const cs = getComputedStyle(body);
    const bodyPad = parseFloat(cs.paddingTop) + parseFloat(cs.paddingBottom);
    const nonBodyChrome = root.offsetHeight - body.offsetHeight;
    // Windows clips a sub-pixel row in the borderless content (and minimal hides
    // overflow, so a shortfall isn't scrollable); a little slack avoids it. macOS
    // fit the content exactly and looked right, so leave it unchanged there.
    const BREATHING_ROOM = isWindows ? 10 : 0;
    const natural = nonBodyChrome + panel.offsetHeight + bodyPad + BREATHING_ROOM;
    const height = Math.min(FULL_HEIGHT, Math.max(200, Math.ceil(natural)));
    fitWindowHeight(win, WINDOW_WIDTH, height).catch(() => {});
  }, [minimal, provider, tab, snapshot]);

  if (!snapshot) {
    return (
      <main className="widget">
        <div className="empty">
          {error ? (
            <p className="err">Couldn’t read usage: {error}</p>
          ) : (
            <p>Scanning local logs…</p>
          )}
        </div>
      </main>
    );
  }

  const { meta, limits, week, models, sessions, providers, glm, kpi } = snapshot;

  // Only show a provider tab when that provider is actually present locally
  // (installed CLI / login, configured key, or local activity). Fall back to
  // showing both if the backend didn't report detection.
  const showClaude = snapshot.detection?.claude ?? true;
  const showGlm = snapshot.detection?.glm ?? true;
  const showCopilot = snapshot.detection?.copilot ?? false;
  // Claude's local-log totals row for the Providers tab.
  const claudeProv = providers.find((p) => p.name.startsWith("Claude")) ?? providers[0];
  const available: ("claude" | "glm" | "copilot")[] = [
    ...(showClaude ? (["claude"] as const) : []),
    ...(showGlm ? (["glm"] as const) : []),
    ...(showCopilot ? (["copilot"] as const) : []),
  ];
  const providerTabs: ("claude" | "glm" | "copilot")[] = available.length
    ? available
    : ["claude", "glm"];
  const eff = providerTabs.includes(provider) ? provider : providerTabs[0];

  return (
    <main className="widget">
      <header className="head">
        <span className="logo">
          <svg viewBox="0 0 24 24" fill="none">
            <circle cx="12" cy="12" r="7.5" stroke="oklch(40% 0.04 220)" strokeWidth="3" />
            <circle
              cx="12" cy="12" r="7.5" fill="none"
              stroke="oklch(82% 0.13 200)" strokeWidth="3" strokeLinecap="round"
              strokeDasharray="32 47" pathLength="47"
              transform="rotate(-90 12 12)"
            />
            <circle cx="12" cy="12" r="2.1" fill="oklch(82% 0.13 200)" />
          </svg>
        </span>
        <div>
          <h1>Agent Usage</h1>
          <div className="sub">updated {meta.generated}</div>
        </div>
        <span className="spacer" />
        {/* The plan tier only sets the ceiling for the *local estimate*. When
            live Claude data is active it reports real limits directly, so the
            selector does nothing — hide it to avoid implying it has an effect. */}
        {!limits.live && (
          <select
            className="plan-select"
            value={plan}
            onChange={(e) => setPlan(e.target.value as PlanKey)}
            title="Plan tier — sets the limit ceilings for the local estimate"
          >
            {PLANS.map((p) => (
              <option key={p.key} value={p.key}>
                {p.label}
              </option>
            ))}
          </select>
        )}
        <button
          className={`refresh ${isLoading ? "spin" : ""}`}
          onClick={() => refresh()}
          title="Refresh now"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round">
            <path d="M21 12a9 9 0 1 1-3-6.7L21 8" />
            <path d="M21 3v5h-5" />
          </svg>
        </button>
      </header>

      <UpdateBanner />

      <nav className="tabs">
        {(["overview", "sessions", "providers", "settings", "about"] as Tab[]).map((t) => (
          <button
            key={t}
            className="tab"
            aria-selected={tab === t}
            onClick={() => setTab(t)}
          >
            {t[0].toUpperCase() + t.slice(1)}
          </button>
        ))}
      </nav>

      <div className={`body${minimal ? " minimal" : ""}`}>
        {tab === "overview" && (
          <section className={`panel prov-${eff}`}>
            {providerTabs.length > 1 && (
              <div className="seg" role="tablist">
                {providerTabs.map((p) => (
                  <button
                    key={p}
                    className="seg-btn"
                    aria-selected={eff === p}
                    onClick={() => setProvider(p)}
                  >
                    <span className={`seg-dot ${p}`} />{" "}
                    {p === "claude" ? "Claude" : p === "glm" ? "GLM" : "Copilot"}
                  </button>
                ))}
              </div>
            )}

            {eff === "claude" && (
              <>
                {limits.needsReauth ? (
                  <div className="connect-card warn">
                    <p className="connect-title">Claude Code login expired</p>
                    <p className="connect-sub">
                      Reconnect to refresh your Claude Code login token in place
                      and restore live usage. Your token &amp; cost totals below
                      are read locally and aren’t affected.
                    </p>
                    <button
                      className="reconnect-btn"
                      disabled={reconnecting}
                      onClick={() => reconnectClaude()}
                    >
                      {reconnecting ? "Reconnecting…" : "Reconnect"}
                    </button>
                    {reconnectError && (
                      <p className="connect-err">
                        Couldn’t reconnect: {reconnectError}. Open Claude Code
                        and run <code>/login</code>, then try again.
                      </p>
                    )}
                  </div>
                ) : limits.pending ? (
                  <div className="connect-card">
                    <p className="connect-title">Reading live Claude usage…</p>
                    <p className="connect-sub">{limits.estimateNote}</p>
                  </div>
                ) : (
                  <>
                    {limits.signedOut && (
                      <div className="notice-bar">
                        <InfoIcon />
                        <span>
                          <b>Not signed in to Claude</b> — showing a local
                          estimate. Run <code>/login</code> in Claude Code for
                          live session &amp; weekly usage.
                        </span>
                      </div>
                    )}
                    <div className="kpis">
                      {limits.buckets.slice(0, 3).map((b, i) => (
                        <div className={`kpi ${["accent", "ok", ""][i]}`} key={b.name}>
                          <div className="k-label">{tileLabel(b.name)}</div>
                          <div className="k-num">{b.usedPct}%</div>
                          <div className="k-sub">resets {b.reset}</div>
                        </div>
                      ))}
                    </div>

                    {!minimal && (
                      <>
                        <div className="sec-head">
                          <h2>Usage</h2>
                          <span className="meta">
                            {limits.live ? "live · Claude" : `${limits.planLabel} plan · est.`}
                          </span>
                        </div>
                        <div className="meters">
                          {limits.buckets.map((b) => (
                            <Meter bucket={b} key={b.name} />
                          ))}
                        </div>
                      </>
                    )}
                  </>
                )}

                {!minimal && (
                  <>
                    <div className="sec-head">
                      <h2>Last 7 days</h2>
                      <span className="meta">tokens / day</span>
                    </div>
                    <WeekChart week={week} />

                    <div className="sec-head">
                      <h2>By model</h2>
                      <span className="meta">all-time tokens</span>
                    </div>
                    <div className="models">
                      {models.map((m) => (
                        <div className="model-row" key={m.key}>
                          <span className="name">{m.name}</span>
                          <div className="mtrack">
                            <div className={`mfill ${m.key}`} style={{ width: `${m.pct}%` }} />
                          </div>
                          <span className="mval">
                            <b>{m.tokens}</b> · {m.cost}
                          </span>
                        </div>
                      ))}
                    </div>

                    {!limits.pending && (
                      <div className="note">
                        <InfoIcon />
                        <p>{limits.estimateNote}</p>
                      </div>
                    )}
                  </>
                )}
              </>
            )}

            {eff === "glm" && (
              <GlmOverview
                vendor={snapshot.vendor?.glm}
                glm={glm}
                minimal={minimal}
                onConnect={() => setTab("settings")}
              />
            )}

            {eff === "copilot" && (
              <CopilotOverview
                vendor={snapshot.vendor?.copilot}
                minimal={minimal}
                onConnect={() => setTab("settings")}
              />
            )}
          </section>
        )}

        {tab === "sessions" && (
          <section className="panel">
            <div className="sec-head">
              <h2>Recent sessions</h2>
              <span className="meta">{meta.filesScanned} logs scanned</span>
            </div>
            <div className="sess">
              {sessions.map((s, i) => (
                <div className="sess-row" key={`${s.id}-${i}`}>
                  <div>
                    <div className="s-proj">{s.project}</div>
                    <div className="s-meta">
                      {s.model && <span className={`badge ${s.model}`}>{s.model}</span>}
                      <span>#{s.id}</span>
                      <span>{s.when}</span>
                    </div>
                  </div>
                  <div>
                    <div className="s-num">{fmtTok(s.tokens)}</div>
                    <div className="s-cost">${s.cost.toFixed(2)}</div>
                  </div>
                </div>
              ))}
            </div>
            <div className="note">
              <InfoIcon />
              <p>
                Tokens include input, output and cache read/write. Cost is estimated from
                standard-tier per-model pricing.
              </p>
            </div>
          </section>
        )}

        {tab === "providers" && (
          <section className="panel">
            <div className="sec-head">
              <h2>Providers</h2>
              <span className="meta">
                {meta.windowFirst} → {meta.windowLast}
              </span>
            </div>
            {/* One self-contained card per provider — local-log totals and live
                API usage merged, so each provider appears exactly once. */}
            <div className="prov">
              <ProviderCard
                status="ok"
                name="Claude Code"
                meta={`${claudeProv?.sessions ?? 0} sessions · local logs`}
                primary={claudeProv?.tokens ?? "—"}
                secondary={claudeProv?.cost}
              />
              {showCopilot && (
                <ProviderCard
                  status={vendorState(snapshot.vendor?.copilot)}
                  name="GitHub Copilot"
                  meta={vendorMeta(snapshot.vendor?.copilot, "not connected")}
                  primary={vendorPrimary(snapshot.vendor?.copilot)}
                />
              )}
              {(showGlm || glm.sessions > 0) && (
                <ProviderCard
                  status={vendorState(snapshot.vendor?.glm)}
                  name="GLM / z.ai"
                  meta={vendorMeta(snapshot.vendor?.glm, "no API key set")}
                  primary={vendorPrimary(snapshot.vendor?.glm)}
                  detail={`${glm.sessions} server sessions · ${glm.activeDays} active days · last ${glm.last}`}
                />
              )}
              <ProviderCard
                status={vendorState(snapshot.vendor?.anthropic)}
                name="Anthropic (org)"
                meta={vendorMeta(snapshot.vendor?.anthropic, "add an admin API key for org cost")}
                primary={vendorPrimary(snapshot.vendor?.anthropic)}
              />
            </div>
            <div className="note">
              <InfoIcon />
              <p>
                Live figures come from each provider’s API; Claude totals and GLM
                activity are read from local logs.
              </p>
            </div>
          </section>
        )}

        {tab === "settings" && settings && (
          <Settings
            settings={settings}
            setApiKey={setApiKey}
            clearApiKey={clearApiKey}
            setGlmEndpoint={setGlmEndpoint}
            setRefreshSecs={setRefreshSecs}
            setLiveClaude={setLiveClaude}
            setLaunchOnStartup={setLaunchOnStartup}
            setTooltipProvider={setTooltipProvider}
            copilotConnected={settings.copilotConnected}
            connectCopilotStart={connectCopilotStart}
            copilotPoll={copilotPoll}
            copilotCancel={copilotCancel}
            disconnectCopilot={disconnectCopilot}
            reloadSettings={reloadSettings}
            setMinimalView={async (enabled) => {
              // Enabling minimal view jumps to Overview so the window shrinks
              // to the compact stats immediately, rather than waiting for the
              // user to leave Settings.
              if (enabled) setTab("overview");
              await setMinimalView(enabled);
            }}
            keyError={keyError}
          />
        )}

        {tab === "about" && <About />}
      </div>

      {!minimal && (
        <footer className="foot">
          <span className="live">
            <span className="pulse" />
            Live · local CLI data
          </span>
          <span>
            {kpi.totalTokens} all-time · {kpi.totalCost}
          </span>
        </footer>
      )}
    </main>
  );
}

/**
 * Quota windows for an API-key vendor (GLM, Copilot), shown the same way as
 * Claude's overview: glanceable brand-tinted tiles up top, then status-colored
 * meter bars under a "Usage" head. Shared so every provider renders identically.
 */
function QuotaMeters({
  windows,
  srcLabel,
  minimal,
  meta,
}: {
  windows: VendorKeyVal[];
  srcLabel: string;
  minimal: boolean;
  meta?: string;
}) {
  if (windows.length === 0) return null;
  return (
    <>
      <div
        className="kpis"
        style={{ gridTemplateColumns: `repeat(${windows.length}, 1fr)` }}
      >
        {windows.map((d, i) => (
          <div className="kpi accent" key={`${d.label}-${i}`}>
            <div className="k-label">{d.label}</div>
            <div className="k-num">{Math.round(d.pct ?? 0)}%</div>
            <div className="k-sub">{d.value || "live"}</div>
          </div>
        ))}
      </div>
      {!minimal && (
        <>
          <div className="sec-head">
            <h2>Usage</h2>
            <span className="meta">{meta ?? `live · ${srcLabel}`}</span>
          </div>
          <div className="meters">
            {windows.map((d, i) => (
              <div className={`meter ${d.status ?? "ok"}`} key={`${d.label}-${i}`}>
                <div className="meter-top">
                  <span className="ml">{d.label}</span>
                  {d.value && <span className="reset">{d.value}</span>}
                </div>
                <div className="track">
                  <div
                    className={`fill ${d.status ?? "ok"}`}
                    style={{ width: `${d.pct ?? 0}%` }}
                  />
                </div>
                <div className="meter-foot">
                  <span className="mu live-tag">● live · {srcLabel}</span>
                  <span className="ml2">
                    <b>{Math.round(d.pct ?? 0)}%</b> used
                  </span>
                </div>
              </div>
            ))}
          </div>
        </>
      )}
    </>
  );
}

function GlmOverview({
  vendor,
  glm,
  minimal,
  onConnect,
}: {
  vendor: VendorStatus | undefined;
  glm: Glm;
  minimal: boolean;
  onConnect: () => void;
}) {
  const live = Boolean(vendor?.configured && vendor.ok);
  // Quota windows carry a pct + status, so they render as glanceable tiles and
  // status-colored meter bars, mirroring Claude's overview.
  const windows = vendor?.detail.filter((d) => d.pct != null) ?? [];

  return (
    <>
      {live && vendor ? (
        <QuotaMeters
          windows={windows}
          srcLabel="z.ai"
          minimal={minimal}
          meta="coding plan · via API key"
        />
      ) : (
        <div className="connect-card">
          <p className="connect-title">
            {vendor?.configured
              ? `Couldn’t reach z.ai${vendor.error ? `: ${vendor.error}` : ""}`
              : "No GLM usage data yet"}
          </p>
          <p className="connect-sub">
            z.ai exposes no per-session tokens locally. Add your GLM Coding Plan API
            key to pull real 5-hour &amp; weekly quota.
          </p>
          <button className="btn primary" onClick={onConnect}>
            Add API key →
          </button>
        </div>
      )}

      {!minimal && (
        <>
          <div className="sec-head">
            <h2>Local activity</h2>
            <span className="meta">MCP logs</span>
          </div>
          <div className="budget">
            <div className="budget-foot" style={{ marginTop: 0 }}>
              <span className="used">{glm.sessions} server sessions</span>
              <span className="rem">{glm.activeDays} active days</span>
            </div>
            <div className="budget-foot">
              <span className="used">last seen</span>
              <span className="rem">{glm.last}</span>
            </div>
          </div>
          <div className="note">
            <InfoIcon />
            <p>{glm.note}</p>
          </div>
        </>
      )}
    </>
  );
}

function CopilotOverview({
  vendor,
  minimal,
  onConnect,
}: {
  vendor: VendorStatus | undefined;
  minimal: boolean;
  onConnect: () => void;
}) {
  const live = Boolean(vendor?.configured && vendor.ok);
  const windows = vendor?.detail.filter((d) => d.pct != null) ?? [];
  // Plan / Resets / Overage (and an "unlimited" plan's premium-requests line)
  // carry no percentage, so they show as plain rows beneath the meters.
  const facts = vendor?.detail.filter((d) => d.pct == null) ?? [];

  return (
    <>
      {live && vendor ? (
        <>
          {windows.length > 0 ? (
            <QuotaMeters
              windows={windows}
              srcLabel="Copilot"
              minimal={minimal}
              meta="live · via Copilot token"
            />
          ) : (
            // Unlimited plan: no quota to meter, so show the headline instead.
            <div className="kpis glm-kpis">
              <div className="kpi accent">
                <div className="k-label">{vendor.secondary || "premium requests"}</div>
                <div className="k-num">{vendor.primary}</div>
                <div className="k-sub">live</div>
              </div>
            </div>
          )}
          {!minimal && facts.length > 0 && (
            <div className="budget" style={{ marginTop: 9 }}>
              {facts.map((d, i) => (
                <div className="budget-foot" key={`${d.label}-${i}`} style={{ marginTop: 0 }}>
                  <span className="used">{d.label}</span>
                  <span className="rem">{d.value}</span>
                </div>
              ))}
            </div>
          )}
        </>
      ) : (
        <div className="connect-card">
          <p className="connect-title">
            {vendor?.configured
              ? `Couldn’t read Copilot usage${vendor.error ? `: ${vendor.error}` : ""}`
              : "No Copilot token found"}
          </p>
          <p className="connect-sub">
            Reads your editor / <code>gh</code> CLI Copilot token to show real
            premium-request quota. If none is found, connect GitHub Copilot to
            authorize this app.
          </p>
          <button className="btn primary" onClick={onConnect}>
            Connect Copilot →
          </button>
        </div>
      )}
    </>
  );
}

// ---- Providers tab: one unified card per provider ----

function vendorState(s?: VendorStatus): "ok" | "err" | "idle" {
  if (!s || !s.configured) return "idle";
  return s.ok ? "ok" : "err";
}

function vendorMeta(s: VendorStatus | undefined, idleText: string): string {
  if (!s || !s.configured) return idleText;
  return s.ok ? s.secondary : s.error || "unavailable";
}

function vendorPrimary(s?: VendorStatus): string {
  return s && s.configured && s.ok ? s.primary : "—";
}

function ProviderCard({
  status,
  name,
  meta,
  primary,
  secondary,
  detail,
}: {
  status: "ok" | "err" | "idle";
  name: string;
  meta: string;
  primary: string;
  secondary?: string;
  detail?: string;
}) {
  return (
    <div className="prov-row stacked">
      <div className="prov-head">
        <span className={`stat ${status}`} />
        <div>
          <div className="pname">{name}</div>
          <div className="pmeta">{meta}</div>
        </div>
        <span className="spacer" />
        <div>
          <div className="pnum">{primary}</div>
          {secondary && <div className="pcost">{secondary}</div>}
        </div>
      </div>
      {detail && <div className="prov-sub">{detail}</div>}
    </div>
  );
}

function InfoIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round">
      <circle cx={12} cy={12} r={10} />
      <path d="M12 16v-4M12 8h.01" />
    </svg>
  );
}

function fmtTok(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(0)}K`;
  return String(n);
}
