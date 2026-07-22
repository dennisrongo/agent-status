import { useCallback, useLayoutEffect, useState, type MouseEvent as ReactMouseEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { About } from "./components/About";
import { Meter } from "./components/Meter";
import { Settings } from "./components/Settings";
import { UpdateBanner } from "./components/UpdateBanner";
import { WeekChart } from "./components/WeekChart";
import { useUsage } from "./hooks/useUsage";
import { fitWindowHeight, isWindows } from "./platform";
import { isTauriReady } from "./tauriReady";
import { generatedLabel, tileLabel } from "./format";
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
    setWindowMode,
    setHiddenProviders,
    reloadSettings,
    connectCopilotStart,
    copilotPoll,
    copilotCancel,
    disconnectCopilot,
    refresh,
    claudeLoginStart,
    claudeLoginFinish,
    claudeLoginCancel,
    claudeLoginBusy,
    claudeLoginError,
    claudeSignOut,
    claudeSignOutError,
    bailianStatus,
    installBailian,
    bailianInstallBusy,
    bailianInstallError,
    loginBailian,
    bailianLoginBusy,
    bailianLoginError,
    isLoading,
    error,
    keyError,
  } = useUsage();
  const [tab, setTab] = useState<Tab>("overview");
  const [provider, setProvider] = useState<"claude" | "glm" | "copilot" | "alibaba">("claude");
  const plan: PlanKey = settings?.plan ?? "max5x";
  // Minimal view only trims the Overview; other tabs always show full content.
  const minimal = (settings?.minimalView ?? false) && tab === "overview";
  const floating = settings?.windowMode === "float";

  // In float mode the header is a drag handle that moves the whole window.
  // `startDragging()` hands the pointer to the OS's native window-drag loop
  // (macOS performWindowDragWithEvent / Win32 WM_NCLBUTTONDOWN), which tracks
  // the move across every monitor at the system level — no per-frame JS, so it
  // stays smooth and never drops mid-drag. Interactive controls in the header
  // (plan select, refresh button) are excluded so they still receive clicks.
  const startWindowDrag = useCallback(
    (e: ReactMouseEvent<HTMLElement>) => {
      if (!floating) return;
      if ((e.target as HTMLElement).closest("button, select, input, a")) return;
      e.preventDefault();
      void getCurrentWindow().startDragging();
    },
    [floating],
  );

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
      fitWindowHeight(win, WINDOW_WIDTH, FULL_HEIGHT, floating).catch(() => {});
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
    fitWindowHeight(win, WINDOW_WIDTH, height, floating).catch(() => {});
  }, [minimal, provider, tab, snapshot, floating]);

  if (!snapshot) {
    return (
      <main className="widget">
        {error ? (
          <div className="empty">
            <p className="err">Couldn’t read usage: {error}</p>
          </div>
        ) : (
          // First snapshot is still being assembled (local log scan + live
          // provider fetches). Show a branded boot screen — spinning logo arc,
          // the four provider dots pulsing in sequence, and shimmering
          // placeholders for the stats about to land — instead of a bare line
          // of text.
          <div className="boot">
            <span className="boot-mark" aria-hidden="true">
              <svg viewBox="0 0 24 24" fill="none">
                <circle cx="12" cy="12" r="7.5" stroke="oklch(36% 0.04 220)" strokeWidth="3" />
                <circle
                  className="boot-arc"
                  cx="12" cy="12" r="7.5" fill="none"
                  stroke="oklch(82% 0.13 200)" strokeWidth="3" strokeLinecap="round"
                  strokeDasharray="12 35" pathLength="47"
                />
                <circle cx="12" cy="12" r="2.1" fill="oklch(82% 0.13 200)" />
              </svg>
            </span>
            <h2 className="boot-title">Warming up</h2>
            <p className="boot-sub">reading local logs · connecting to providers</p>
            <div className="boot-provs" aria-hidden="true">
              <span className="boot-dot claude" />
              <span className="boot-dot glm" />
              <span className="boot-dot copilot" />
              <span className="boot-dot alibaba" />
            </div>
            <div className="boot-skel" aria-hidden="true">
              <div className="skel-kpis">
                <span className="sk" />
                <span className="sk" />
                <span className="sk" />
              </div>
              <span className="sk skel-line" />
              <span className="sk skel-line short" />
            </div>
          </div>
        )}
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
  const showAlibaba = snapshot.detection?.alibaba ?? false;
  // A present, non-expired Claude Code login is required to show ANY Claude
  // usage now — the local estimate included. Without it (signed out or expired)
  // the Overview shows a connect/reconnect prompt instead of stats. Detection
  // reflects the login independent of the live toggle, so this also subsumes the
  // live-mode signed-out / needs-reauth states the backend flags. Default to
  // "connected" if a snapshot ever lacks detection, so we never blank a valid
  // reading on missing data.
  const claudeExpired = snapshot.detection?.claudeExpired ?? false;
  const claudeConnected = snapshot.detection
    ? snapshot.detection.claudeSignedIn && !claudeExpired
    : true;
  // Claude's local-log totals row for the Providers tab.
  const claudeProv = providers.find((p) => p.name.startsWith("Claude")) ?? providers[0];
  const hidden = new Set(settings?.hiddenProviders ?? []);
  const available: ("claude" | "glm" | "copilot" | "alibaba")[] = [
    ...(showClaude ? (["claude"] as const) : []),
    ...(showGlm ? (["glm"] as const) : []),
    ...(showCopilot ? (["copilot"] as const) : []),
    ...(showAlibaba ? (["alibaba"] as const) : []),
  ];
  const visible = available.filter((p) => !hidden.has(p));
  const providerTabs: ("claude" | "glm" | "copilot" | "alibaba")[] = visible.length
    ? visible
    : available.length
      ? available
      : ["claude", "glm"];
  const allHidden = available.length > 0 && visible.length === 0;
  const eff = providerTabs.includes(provider) ? provider : providerTabs[0];

  return (
    <main className="widget">
      <header
        className={`head${floating ? " float-drag" : ""}`}
        onMouseDown={floating ? startWindowDrag : undefined}
      >
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
          <div className="sub">updated {generatedLabel(meta.generatedMs, meta.generated)}</div>
        </div>
        <span className="spacer" />
        {/* The plan tier only sets the ceiling for the *local estimate*. When
            live Claude data is active it reports real limits directly, so the
            selector does nothing — hide it to avoid implying it has an effect. */}
        {!limits.live && claudeConnected && (
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
            {allHidden ? (
              <div className="connect-card">
                <p className="connect-title">All providers hidden</p>
                <p className="connect-sub">
                  Every detected provider is hidden from the Overview. Re-enable
                  one in Settings → Providers.
                </p>
                <button className="btn primary" onClick={() => setTab("settings")}>
                  Open Settings →
                </button>
              </div>
            ) : (
              <>
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
                    {p === "claude" ? "Claude" : p === "glm" ? "GLM" : p === "copilot" ? "Copilot" : "Alibaba"}
                  </button>
                ))}
              </div>
            )}

            {eff === "claude" &&
              (!claudeConnected ? (
                <ClaudeConnectPrompt
                  expired={claudeExpired}
                  onConnect={() => setTab("settings")}
                />
              ) : (
                <>
                  {limits.pending ? (
                    <div className="connect-card">
                      <p className="connect-title">Reading live Claude usage…</p>
                      <p className="connect-sub">{limits.estimateNote}</p>
                    </div>
                  ) : (
                    <>
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
              ))}

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

            {eff === "alibaba" && (
              <AlibabaOverview
                vendor={snapshot.vendor?.alibaba}
                minimal={minimal}
                onConnect={() => setTab("settings")}
                onLogin={() => void loginBailian()}
                loginBusy={bailianLoginBusy}
                loginError={bailianLoginError}
              />
            )}
              </>
            )}
          </section>
        )}

        {tab === "sessions" && (
          <section className="panel">
            <div className="sec-head">
              <h2>Recent activity</h2>
              <span className="meta">{meta.filesScanned} logs scanned</span>
            </div>
            <div className="sess">
              {sessions.map((s) => {
                const isGlm = s.provider === "glm";
                return (
                  <div className="sess-row" key={`${s.provider}-${s.id}`}>
                    <div className="s-main">
                      <div className="s-proj">
                        <span className={`s-prov ${s.provider}`} />
                        <span className="s-name">{s.project}</span>
                      </div>
                      <div className="s-meta">
                        {s.model && <span className={`badge ${s.model}`}>{s.model}</span>}
                        {isGlm ? (
                          <span>{glm.sessions} session{glm.sessions === 1 ? "" : "s"}</span>
                        ) : (
                          <span>#{s.id}</span>
                        )}
                        <span>{s.when}</span>
                      </div>
                    </div>
                    <div>
                      <div className="s-num">{isGlm ? "—" : fmtTok(s.tokens)}</div>
                      <div className="s-cost">{isGlm ? "—" : `$${s.cost.toFixed(2)}`}</div>
                    </div>
                  </div>
                );
              })}
            </div>
            <div className="note">
              <InfoIcon />
              <p>
                Activity spans all providers with local session data. Claude tokens include
                input, output and cache read/write (cost estimated from standard-tier pricing).
                GLM logs are server-lifecycle only. Copilot usage is tracked on the Providers tab.
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
              {showAlibaba && (
                <ProviderCard
                  status={vendorState(snapshot.vendor?.alibaba)}
                  name="Alibaba Cloud"
                  meta={vendorMeta(snapshot.vendor?.alibaba, "install the Bailian CLI (bl)")}
                  primary={vendorPrimary(snapshot.vendor?.alibaba)}
                />
              )}
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
            claudeSignedIn={snapshot?.detection?.claudeSignedIn ?? false}
            claudeExpired={snapshot?.detection?.claudeExpired ?? false}
            claudeSignOut={claudeSignOut}
            claudeSignOutError={claudeSignOutError}
            claudeLoginStart={claudeLoginStart}
            claudeLoginFinish={claudeLoginFinish}
            claudeLoginCancel={claudeLoginCancel}
            claudeLoginBusy={claudeLoginBusy}
            claudeLoginError={claudeLoginError}
            setLaunchOnStartup={setLaunchOnStartup}
            setTooltipProvider={setTooltipProvider}
            setWindowMode={setWindowMode}
            setHiddenProviders={setHiddenProviders}
            copilotConnected={settings.copilotConnected}
            connectCopilotStart={connectCopilotStart}
            copilotPoll={copilotPoll}
            copilotCancel={copilotCancel}
            disconnectCopilot={disconnectCopilot}
            reloadSettings={reloadSettings}
            bailianStatus={bailianStatus}
            installBailian={installBailian}
            bailianInstallBusy={bailianInstallBusy}
            bailianInstallError={bailianInstallError}
            loginBailian={loginBailian}
            bailianLoginBusy={bailianLoginBusy}
            bailianLoginError={bailianLoginError}
            alibabaVendorStatus={snapshot?.vendor?.alibaba}
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
          {/* All-time totals are Claude local-log figures — hide them when
              there's no usable Claude login, same as the rest of its stats. */}
          {claudeConnected && (
            <span>
              {kpi.totalTokens} all-time · {kpi.totalCost}
            </span>
          )}
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
  // Per-tool breakdown rows (usageDetails) are plain text — no pct.
  const toolRows = vendor?.detail.filter((d) => d.pct == null) ?? [];

  return (
    <>
      {live && vendor ? (
        <>
          <QuotaMeters
            windows={windows}
            srcLabel="z.ai"
            minimal={minimal}
            meta="coding plan · via API key"
          />
          {!minimal && toolRows.length > 0 && (
            <>
              <div className="sec-head">
                <h2>Tool breakdown</h2>
                <span className="meta">monthly quota</span>
              </div>
              <div className="budget">
                {toolRows.map((d, i) => (
                  <div className="budget-foot" key={`${d.label}-${i}`} style={i === 0 ? { marginTop: 0 } : undefined}>
                    <span className="used">{d.label}</span>
                    <span className="rem">{d.value}</span>
                  </div>
                ))}
              </div>
            </>
          )}
        </>
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

      {/* MCP-log activity is GLM data, so it's shown only when GLM is connected
          (a key is configured). With no key the provider is signed out — just
          the connect card, no data — matching every other provider. */}
      {!minimal && vendor?.configured && (
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

function AlibabaOverview({
  vendor,
  minimal,
  onConnect,
  onLogin,
  loginBusy,
  loginError,
}: {
  vendor: VendorStatus | undefined;
  minimal: boolean;
  onConnect: () => void;
  onLogin: () => void;
  loginBusy: boolean;
  loginError: string | null;
}) {
  const live = Boolean(vendor?.configured && vendor.ok);
  const all = vendor?.detail ?? [];
  const windows = all.filter((d) => d.pct != null);
  const windowRows = all.filter((d) => d.pct == null && (d.label === "Today" || d.label === "7 days"));
  const facts = all.filter((d) => d.pct == null && d.label !== "Today" && d.label !== "7 days");

  return (
    <>
      {live && vendor ? (
        <>
          {windows.length > 0 ? (
            <QuotaMeters
              windows={windows}
              srcLabel="Bailian"
              minimal={minimal}
              meta="live · token plan"
            />
          ) : windowRows.length > 0 ? (
            <div
              className="kpis"
              style={{ gridTemplateColumns: `repeat(${windowRows.length}, 1fr)` }}
            >
              {windowRows.map((d, i) => (
                <div className="kpi accent" key={`${d.label}-${i}`}>
                  <div className="k-label">{d.label}</div>
                  <div className="k-num">{d.value.split("·")[0].trim()}</div>
                  <div className="k-sub">{d.value.split("·").slice(1).join("·").trim() || "live"}</div>
                </div>
              ))}
            </div>
          ) : (
            <div className="kpis glm-kpis">
              <div className="kpi accent">
                <div className="k-label">{vendor.secondary || "usage"}</div>
                <div className="k-num">{vendor.primary}</div>
                <div className="k-sub">live</div>
              </div>
            </div>
          )}
          {!minimal && facts.length > 0 && (
            <>
              <div className="sec-head">
                <h2>{windows.length > 0 ? "Stats" : "Usage"}</h2>
                <span className="meta">live · Bailian CLI</span>
              </div>
              <div className="budget" style={{ marginTop: 9 }}>
                {facts.map((d, i) => (
                  <div className="budget-foot" key={`${d.label}-${i}`} style={{ marginTop: 0 }}>
                    <span className="used">{d.label}</span>
                    <span className="rem">{d.value}</span>
                  </div>
                ))}
              </div>
            </>
          )}
        </>
      ) : vendor?.authExpired ? (
        // The CLI is installed and `bl auth status` reports a credential, but
        // the *console session* has expired — usage calls fail with code 3.
        // This is terminal (retrying won't help until the user re-logs-in), so
        // show a reconnect card instead of the indefinite "Connecting…"
        // spinner. Mirrors ClaudeConnectPrompt's expired branch.
        <div className="connect-card warn">
          <p className="connect-title">Alibaba Cloud session expired</p>
          <p className="connect-sub">
            The Bailian CLI&rsquo;s console session has expired. Sign in again to
            restore your usage and quota.
          </p>
          {vendor.error && <p className="connect-hint">{vendor.error}</p>}
          <button className="btn primary" disabled={loginBusy} onClick={onLogin}>
            {loginBusy ? "Signing in…" : "Sign in to Alibaba Cloud"}
          </button>
          {loginError && <p className="key-err">{loginError}</p>}
        </div>
      ) : vendor?.configured ? (
        // The CLI is installed but this fetch failed and there's no recent
        // reading to fall back on. The background loop keeps retrying, so show
        // a calm "connecting" state instead of dumping the raw CLI error.
        <div className="connect-card connecting">
          <div className="connect-anim" aria-hidden="true">
            <span className="ping" />
            <span className="ping p2" />
            <span className="core" />
          </div>
          <p className="connect-title">Connecting to Alibaba Cloud…</p>
          <p className="connect-sub">
            Reading your Bailian usage — this can take a moment. We&rsquo;ll keep
            retrying in the background.
          </p>
          {vendor.error && <p className="connect-hint">{vendor.error}</p>}
          <button className="btn" onClick={onConnect}>
            Setup guide →
          </button>
        </div>
      ) : (
        <div className="connect-card">
          <p className="connect-title">No Alibaba Cloud usage data yet</p>
          <p className="connect-sub">
            Install the Bailian CLI (<code>npm i -g bailian-cli</code>) and run{" "}
            <code>bl auth login --console</code> to see token usage and quota.
          </p>
          <button className="btn primary" onClick={onConnect}>
            Setup guide →
          </button>
        </div>
      )}
    </>
  );
}

/**
 * Shown in place of all Claude usage when there's no usable Claude Code login —
 * either none at all (signed out) or one that's expired. A present, valid login
 * is required to show Claude stats now (local estimate included), so this stands
 * in for the meters / tiles / history with a single connect-or-reconnect prompt.
 */
function ClaudeConnectPrompt({
  expired,
  onConnect,
}: {
  expired: boolean;
  onConnect: () => void;
}) {
  return (
    <div className={`connect-card${expired ? " warn" : ""}`}>
      <p className="connect-title">
        {expired ? "Claude login expired" : "Not connected to Claude"}
      </p>
      <p className="connect-sub">
        {expired
          ? "Reconnect to see your session limits, token usage, and history."
          : "Sign in to Claude to see your session limits, token usage, and history."}
      </p>
      <button className="reconnect-btn" onClick={onConnect}>
        {expired ? "Reconnect in Settings →" : "Connect in Settings →"}
      </button>
    </div>
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
