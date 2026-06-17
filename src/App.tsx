import { useState } from "react";

import { Meter } from "./components/Meter";
import { Settings } from "./components/Settings";
import { VendorCard } from "./components/VendorCard";
import { WeekChart } from "./components/WeekChart";
import { useUsage } from "./hooks/useUsage";
import type { PlanKey } from "./types";

type Tab = "overview" | "sessions" | "providers" | "settings";

const PLANS: { key: PlanKey; label: string }[] = [
  { key: "pro", label: "Pro" },
  { key: "max5x", label: "Max 5×" },
  { key: "max20x", label: "Max 20×" },
];

export default function App() {
  const { snapshot, plan, setPlan, refresh, isLoading, error } = useUsage();
  const [tab, setTab] = useState<Tab>("overview");

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
        {(["overview", "sessions", "providers"] as Tab[]).map((t) => (
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
            <div className="kpis">
              <div className="kpi accent">
                <div className="k-label">Session left</div>
                <div className="k-num">{limits.buckets[0].leftPct}%</div>
                <div className="k-sub">resets {limits.buckets[0].reset}</div>
              </div>
              <div className="kpi ok">
                <div className="k-label">Week left</div>
                <div className="k-num">{limits.buckets[1].leftPct}%</div>
                <div className="k-sub">all models</div>
              </div>
              <div className="kpi">
                <div className="k-label">Opus left</div>
                <div className="k-num">{limits.buckets[2].leftPct}%</div>
                <div className="k-sub">resets {limits.buckets[2].reset}</div>
              </div>
            </div>

            <div className="sec-head">
              <h2>What’s left</h2>
              <span className="meta">{limits.planLabel} plan</span>
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
