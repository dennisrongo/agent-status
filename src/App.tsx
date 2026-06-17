import { useState } from "react";

import { Meter } from "./components/Meter";
import { Settings } from "./components/Settings";
import { VendorCard } from "./components/VendorCard";
import { WeekChart } from "./components/WeekChart";
import { useUsage } from "./hooks/useUsage";
import type { Glm, PlanKey, VendorStatus } from "./types";

type Tab = "overview" | "sessions" | "providers" | "settings";

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
    refresh,
    isLoading,
    error,
    keyError,
  } = useUsage();
  const [tab, setTab] = useState<Tab>("overview");
  const [provider, setProvider] = useState<"claude" | "glm">("claude");
  const plan: PlanKey = settings?.plan ?? "max5x";

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
  const available: ("claude" | "glm")[] = [
    ...(showClaude ? (["claude"] as const) : []),
    ...(showGlm ? (["glm"] as const) : []),
  ];
  const providerTabs = available.length ? available : (["claude", "glm"] as const);
  const eff = providerTabs.includes(provider) ? provider : providerTabs[0];

  return (
    <main className="widget">
      <header className="head">
        <span className="logo">
          <svg viewBox="0 0 24 24" fill="none" stroke="oklch(82% 0.13 200)" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round">
            <path d="M3 3v18h18" />
            <path d="M7 14l4-4 3 3 5-6" />
          </svg>
        </span>
        <div>
          <h1>Agent Usage</h1>
          <div className="sub">updated {meta.generated}</div>
        </div>
        <span className="spacer" />
        <select
          className="plan-select"
          value={plan}
          onChange={(e) => setPlan(e.target.value as PlanKey)}
          title="Plan tier — sets the limit ceilings"
        >
          {PLANS.map((p) => (
            <option key={p.key} value={p.key}>
              {p.label}
            </option>
          ))}
        </select>
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

      <nav className="tabs">
        {(["overview", "sessions", "providers", "settings"] as Tab[]).map((t) => (
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

      <div className="body">
        {tab === "overview" && (
          <section className="panel">
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
                    {p === "claude" ? "Claude" : "GLM"}
                  </button>
                ))}
              </div>
            )}

            {eff === "claude" && (
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

                <div className="sec-head">
                  <h2>Usage</h2>
                  <span className="meta">
                    {limits.planLabel === "live" ? "live · Claude" : `${limits.planLabel} plan · est.`}
                  </span>
                </div>
                <div className="meters">
                  {limits.buckets.map((b) => (
                    <Meter bucket={b} key={b.name} />
                  ))}
                </div>

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

                <div className="note">
                  <InfoIcon />
                  <p>{limits.estimateNote}</p>
                </div>
              </>
            )}

            {eff === "glm" && (
              <GlmOverview
                vendor={snapshot.vendor?.glm}
                glm={glm}
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
              <h2>Connected providers</h2>
              <span className="meta">
                {meta.windowFirst} → {meta.windowLast}
              </span>
            </div>
            <div className="prov">
              {providers.map((p) => (
                <div className="prov-row" key={p.name}>
                  <span className="stat" />
                  <div>
                    <div className="pname">{p.name}</div>
                    <div className="pmeta">
                      {p.sessions} sessions · {p.status}
                    </div>
                  </div>
                  <span className="spacer" />
                  <div>
                    <div className="pnum">{p.tokens}</div>
                    <div className="pcost">{p.cost}</div>
                  </div>
                </div>
              ))}
            </div>

            {snapshot.vendor && (
              <>
                <div className="sec-head">
                  <h2>Live vendor usage</h2>
                  <span className="meta">via API key</span>
                </div>
                <div className="prov">
                  <VendorCard name="Anthropic (org)" status={snapshot.vendor.anthropic} />
                  <VendorCard name="z.ai (GLM)" status={snapshot.vendor.glm} />
                </div>
              </>
            )}

            <div className="sec-head">
              <h2>GLM / z.ai detail</h2>
              <span className="meta">local MCP logs</span>
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
            keyError={keyError}
          />
        )}
      </div>

      <footer className="foot">
        <span className="live">
          <span className="pulse" />
          Live · local CLI data
        </span>
        <span>
          {kpi.totalTokens} all-time · {kpi.totalCost}
        </span>
      </footer>
    </main>
  );
}

function GlmOverview({
  vendor,
  glm,
  onConnect,
}: {
  vendor: VendorStatus | undefined;
  glm: Glm;
  onConnect: () => void;
}) {
  const live = Boolean(vendor?.configured && vendor.ok);

  return (
    <>
      <div className="sec-head">
        <h2>z.ai quota</h2>
        <span className="meta">coding plan · via API key</span>
      </div>

      {live && vendor ? (
        <>
          <div className="kpis glm-kpis">
            <div className="kpi accent">
              <div className="k-label">{vendor.secondary || "balance"}</div>
              <div className="k-num">{vendor.primary}</div>
              <div className="k-sub">live</div>
            </div>
          </div>
          {vendor.detail.length > 0 && (
            <div className="budget" style={{ marginTop: 9 }}>
              {vendor.detail.map((d) => (
                <div className="budget-foot" key={d.label} style={{ marginTop: 0 }}>
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

function tileLabel(name: string): string {
  if (name.startsWith("Session")) return "Session";
  if (name.includes("all models")) return "Week";
  const scope = name.split("·").pop()?.trim();
  return scope ? `${scope} wk` : name;
}

function fmtTok(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(0)}K`;
  return String(n);
}
