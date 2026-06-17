import { useState } from "react";

import type { SettingsView } from "../types";

interface Props {
  settings: SettingsView;
  setApiKey: (provider: "glm" | "anthropic", key: string) => Promise<SettingsView | null>;
  clearApiKey: (provider: "glm" | "anthropic") => Promise<void>;
  setGlmEndpoint: (endpoint: string) => Promise<void>;
  setRefreshSecs: (secs: number) => Promise<void>;
  setLiveClaude: (enabled: boolean) => Promise<void>;
  keyError: string | null;
}

const REFRESH_OPTIONS = [
  { secs: 10, label: "10 seconds" },
  { secs: 15, label: "15 seconds" },
  { secs: 30, label: "30 seconds" },
  { secs: 60, label: "1 minute" },
  { secs: 120, label: "2 minutes" },
  { secs: 300, label: "5 minutes" },
];

export function Settings({
  settings,
  setApiKey,
  clearApiKey,
  setGlmEndpoint,
  setRefreshSecs,
  setLiveClaude,
  keyError,
}: Props) {
  return (
    <section className="panel">
      <div className="sec-head">
        <h2>Claude usage</h2>
        <span className="meta">{settings.liveClaude ? "live" : "estimate"}</span>
      </div>
      <div className="key-row">
        <label className="toggle-row">
          <span>
            <span className="key-label">Live usage from Claude Code</span>
            <span className="connect-sub" style={{ margin: "4px 0 0" }}>
              Reads your Claude Code login to show real session/weekly %. Off = local token estimate.
            </span>
          </span>
          <input
            type="checkbox"
            className="toggle"
            checked={settings.liveClaude}
            onChange={(e) => setLiveClaude(e.target.checked)}
          />
        </label>
      </div>

      <div className="sec-head">
        <h2>Auto-refresh</h2>
        <span className="meta">every {settings.refreshSecs}s</span>
      </div>
      <div className="key-row">
        <div className="key-top">
          <span className="key-label">Refresh interval</span>
        </div>
        <select
          className="interval-select"
          value={refreshValue(settings.refreshSecs)}
          onChange={(e) => setRefreshSecs(Number(e.target.value))}
        >
          {REFRESH_OPTIONS.map((o) => (
            <option key={o.secs} value={o.secs}>
              {o.label}
            </option>
          ))}
        </select>
      </div>

      <div className="sec-head">
        <h2>API keys</h2>
        <span className="meta">stored encrypted</span>
      </div>

      <KeyRow
        label="z.ai (GLM)"
        hint="paste your GLM Coding Plan token"
        sub="From your GLM Coding Plan subscription — used to pull real 5-hour & weekly quota. A standard pay-as-you-go API key won't return plan usage."
        isSet={settings.glmKeySet}
        onSave={(k) => setApiKey("glm", k)}
        onClear={() => clearApiKey("glm")}
      />

      <KeyRow
        label="Anthropic admin"
        hint="sk-ant-admin… — org usage/cost (not the subscription %)"
        isSet={settings.anthropicKeySet}
        onSave={(k) => setApiKey("anthropic", k)}
        onClear={() => clearApiKey("anthropic")}
      />

      {keyError && <p className="key-err">{keyError}</p>}

      <div className="sec-head">
        <h2>z.ai endpoint</h2>
        <span className="meta">verify for your account</span>
      </div>
      <EndpointRow value={settings.glmEndpoint} onSave={setGlmEndpoint} />

      <div className="note">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round">
          <circle cx={12} cy={12} r={10} />
          <path d="M12 16v-4M12 8h.01" />
        </svg>
        <p>
          Keys are encrypted (AES-256-GCM) and bound to this machine — they never
          leave Rust in plaintext. The Anthropic admin API reports org-level
          token/cost, which is not the Pro/Max weekly limit.
        </p>
      </div>
    </section>
  );
}

// Snap a stored interval to the nearest preset so the select always shows a value.
function refreshValue(secs: number): number {
  return REFRESH_OPTIONS.reduce((best, o) =>
    Math.abs(o.secs - secs) < Math.abs(best.secs - secs) ? o : best,
  ).secs;
}

function KeyRow({
  label,
  hint,
  sub,
  isSet,
  onSave,
  onClear,
}: {
  label: string;
  hint: string;
  sub?: string;
  isSet: boolean;
  onSave: (key: string) => Promise<unknown>;
  onClear: () => Promise<void>;
}) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);

  return (
    <div className="key-row">
      <div className="key-top">
        <span className="key-label">{label}</span>
        <span className={`key-status ${isSet ? "set" : ""}`}>
          {isSet ? "● set" : "○ not set"}
        </span>
      </div>
      {sub && (
        <span className="connect-sub" style={{ margin: "0 0 6px" }}>
          {sub}
        </span>
      )}
      <div className="key-input">
        <input
          type="password"
          placeholder={isSet ? "••••••• (saved) — enter to replace" : hint}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          autoComplete="off"
          spellCheck={false}
        />
        <button
          className="btn primary"
          disabled={busy || value.trim().length === 0}
          onClick={async () => {
            setBusy(true);
            const ok = await onSave(value.trim());
            setBusy(false);
            if (ok) setValue("");
          }}
        >
          Save
        </button>
        {isSet && (
          <button
            className="btn"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              await onClear();
              setBusy(false);
            }}
          >
            Clear
          </button>
        )}
      </div>
    </div>
  );
}

function EndpointRow({ value, onSave }: { value: string; onSave: (v: string) => Promise<void> }) {
  const [v, setV] = useState(value);
  const [busy, setBusy] = useState(false);
  return (
    <div className="key-input">
      <input
        type="text"
        value={v}
        onChange={(e) => setV(e.target.value)}
        spellCheck={false}
        autoComplete="off"
      />
      <button
        className="btn"
        disabled={busy || v.trim().length === 0 || v === value}
        onClick={async () => {
          setBusy(true);
          await onSave(v.trim());
          setBusy(false);
        }}
      >
        Save
      </button>
    </div>
  );
}
