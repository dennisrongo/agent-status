import { useEffect, useRef, useState } from "react";

import type { CopilotDeviceCode, SettingsView, TooltipProvider } from "../types";

interface Props {
  settings: SettingsView;
  setApiKey: (provider: "glm" | "anthropic", key: string) => Promise<SettingsView | null>;
  clearApiKey: (provider: "glm" | "anthropic") => Promise<void>;
  setGlmEndpoint: (endpoint: string) => Promise<void>;
  setRefreshSecs: (secs: number) => Promise<void>;
  setLiveClaude: (enabled: boolean) => Promise<void>;
  setLaunchOnStartup: (enabled: boolean) => Promise<void>;
  setMinimalView: (enabled: boolean) => Promise<void>;
  setTooltipProvider: (provider: TooltipProvider) => Promise<void>;
  copilotConnected: boolean;
  connectCopilotStart: () => Promise<CopilotDeviceCode | null>;
  copilotPoll: () => Promise<string | null>;
  copilotCancel: () => void;
  disconnectCopilot: () => Promise<void>;
  reloadSettings: () => Promise<void>;
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
  setLaunchOnStartup,
  setMinimalView,
  setTooltipProvider,
  copilotConnected,
  connectCopilotStart,
  copilotPoll,
  copilotCancel,
  disconnectCopilot,
  reloadSettings,
  keyError,
}: Props) {
  return (
    <section className="panel">
      <div className="group-head">General</div>
      <div className="sec-head">
        <h2>Display</h2>
        <span className="meta">{settings.minimalView ? "minimal" : "full"}</span>
      </div>
      <div className="key-row">
        <label className="toggle-row">
          <span>
            <span className="key-label">Minimal view</span>
            <span className="connect-sub" style={{ margin: "4px 0 0" }}>
              Show only the headline stats on Overview and shrink the window to fit — no scrolling. Off shows the full breakdown.
            </span>
          </span>
          <input
            type="checkbox"
            className="toggle"
            checked={settings.minimalView}
            onChange={(e) => setMinimalView(e.target.checked)}
          />
        </label>
      </div>
      <div className="key-row">
        <div className="key-top">
          <span className="key-label">Tray hover provider</span>
        </div>
        <span className="connect-sub" style={{ margin: "0 0 6px" }}>
          Which provider's usage the menu-bar hover popover previews.
        </span>
        <select
          className="interval-select"
          value={settings.tooltipProvider}
          onChange={(e) => setTooltipProvider(e.target.value as TooltipProvider)}
        >
          <option value="claude">Claude</option>
          <option value="glm">GLM / z.ai</option>
          <option value="copilot">GitHub Copilot</option>
        </select>
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
        <h2>Startup</h2>
        <span className="meta">{settings.launchOnStartup ? "on" : "off"}</span>
      </div>
      <div className="key-row">
        <label className="toggle-row">
          <span>
            <span className="key-label">Launch at login</span>
            <span className="connect-sub" style={{ margin: "4px 0 0" }}>
              Start Agent Usage Monitor automatically when you log in.
            </span>
          </span>
          <input
            type="checkbox"
            className="toggle"
            checked={settings.launchOnStartup}
            onChange={(e) => setLaunchOnStartup(e.target.checked)}
          />
        </label>
      </div>

      <div className="group-head">Providers</div>
      <div className="sec-head">
        <h2>Claude / Anthropic</h2>
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
      <KeyRow
        label="Anthropic admin API key"
        hint="sk-ant-admin… — org-level API cost"
        sub="Org-level API cost via the Anthropic Admin API — separate from the Claude Code subscription usage above (not your weekly % limit)."
        isSet={settings.anthropicKeySet}
        onSave={(k) => setApiKey("anthropic", k)}
        onClear={() => clearApiKey("anthropic")}
      />

      <div className="sec-head">
        <h2>GitHub Copilot</h2>
        <span className="meta">{copilotConnected ? "connected" : "auto / connect"}</span>
      </div>
      <CopilotConnect
        connected={copilotConnected}
        start={connectCopilotStart}
        poll={copilotPoll}
        cancel={copilotCancel}
        disconnect={disconnectCopilot}
        onConnected={reloadSettings}
      />

      <div className="sec-head">
        <h2>GLM / z.ai</h2>
        <span className="meta">stored encrypted</span>
      </div>
      <KeyRow
        label="API key"
        hint="paste your GLM Coding Plan token"
        sub="From your GLM Coding Plan subscription — used to pull real 5-hour & weekly quota. A standard pay-as-you-go API key won't return plan usage."
        isSet={settings.glmKeySet}
        onSave={(k) => setApiKey("glm", k)}
        onClear={() => clearApiKey("glm")}
      />
      <div className="key-row">
        <div className="key-top">
          <span className="key-label">Endpoint</span>
        </div>
        <span className="connect-sub" style={{ margin: "0 0 6px" }}>
          Usage API endpoint — verify it for your account / region.
        </span>
        <EndpointRow value={settings.glmEndpoint} onSave={setGlmEndpoint} />
      </div>

      {keyError && <p className="key-err">{keyError}</p>}

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

function CopilotConnect({
  connected,
  start,
  poll,
  cancel,
  disconnect,
  onConnected,
}: {
  connected: boolean;
  start: () => Promise<CopilotDeviceCode | null>;
  poll: () => Promise<string | null>;
  cancel: () => void;
  disconnect: () => Promise<void>;
  onConnected: () => Promise<void>;
}) {
  const [code, setCode] = useState<CopilotDeviceCode | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Monotonic id of the active poll chain. Bumping it invalidates any chain
  // started earlier (a second Connect click, a Cancel, or unmount), so exactly
  // one chain is ever live — no orphaned pollers spinning against a device code
  // the backend has already cleared.
  const runId = useRef(0);

  // Stop polling if the user navigates away mid-flow.
  useEffect(() => {
    return () => {
      runId.current++;
    };
  }, []);

  const begin = async () => {
    const myRun = ++runId.current;
    setBusy(true);
    setMsg(null);
    const info = await start();
    setBusy(false);
    if (runId.current !== myRun) return; // superseded while awaiting start
    if (!info) {
      setMsg("Couldn’t start the connection — try again.");
      return;
    }
    setCode(info);
    const baseMs = Math.max(2, info.interval) * 1000;
    const tick = async (intervalMs: number) => {
      if (runId.current !== myRun) return;
      const status = await poll();
      if (runId.current !== myRun) return;
      if (status === "connected") {
        setCode(null);
        await onConnected();
        return;
      }
      if (status === "pending") {
        window.setTimeout(() => tick(intervalMs), intervalMs);
        return;
      }
      if (status === "slow_down") {
        // Per the OAuth device-flow spec, add 5s to the interval on slow_down
        // and keep the slower cadence for the rest of the flow.
        const slower = intervalMs + 5000;
        window.setTimeout(() => tick(slower), slower);
        return;
      }
      // Terminal: denied / expired / a swallowed backend error (null) / anything
      // unexpected. Never re-schedule — re-scheduling on a non-"pending" status
      // is exactly how an orphaned chain could poll forever.
      setCode(null);
      setMsg(
        status === "denied"
          ? "Authorization was denied."
          : status === "expired"
            ? "The code expired — try connecting again."
            : "Connection stopped — try connecting again.",
      );
    };
    window.setTimeout(() => tick(baseMs), baseMs);
  };

  const abort = () => {
    runId.current++; // invalidate the running chain locally…
    setCode(null);
    cancel(); // …and drop the pending device code server-side, so a later
    // Connect mints a fresh code instead of re-handing this dismissed one.
  };

  if (connected) {
    return (
      <div className="key-row">
        <div className="key-top">
          <span className="key-label">Connected</span>
          <span className="key-status set">● connected</span>
        </div>
        <span className="connect-sub" style={{ margin: "0 0 8px" }}>
          Using the Copilot token you connected here.
        </span>
        <button
          className="btn"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            await disconnect();
            setBusy(false);
          }}
        >
          Disconnect
        </button>
      </div>
    );
  }

  if (code) {
    return (
      <div className="key-row">
        <span className="connect-sub" style={{ margin: "0 0 6px" }}>
          A browser opened to{" "}
          <code>{code.verificationUri.replace(/^https?:\/\//, "")}</code>. Enter this
          code to authorize, then come back — this updates automatically.
        </span>
        <div className="key-top">
          <span className="key-label" style={{ fontFamily: "var(--mono)", fontSize: 16, letterSpacing: "0.1em" }}>
            {code.userCode}
          </span>
          <button className="btn" onClick={abort}>
            Cancel
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="key-row">
      <span className="connect-sub" style={{ margin: "0 0 8px" }}>
        Usage is read automatically from your editor / <code>gh</code> CLI Copilot
        token. Only connect here if no token is found automatically.
      </span>
      <button className="btn primary" disabled={busy} onClick={begin}>
        {busy ? "Starting…" : "Connect GitHub Copilot"}
      </button>
      <span className="connect-sub" style={{ margin: "8px 0 0", color: "var(--faint)" }}>
        Authorizes via GitHub’s device flow using VS Code Copilot’s client ID,
        with <code>read:user</code> scope — the session shows as “VS Code” in your
        GitHub audit log. Disconnect any time above.
      </span>
      {msg && <p className="key-err">{msg}</p>}
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
