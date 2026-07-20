import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { BailianCliStatus, ClaudeLoginInfo, CopilotDeviceCode, SettingsView, TooltipProvider, WindowMode } from "../types";

interface Props {
  settings: SettingsView;
  setApiKey: (provider: "glm" | "anthropic", key: string) => Promise<SettingsView | null>;
  clearApiKey: (provider: "glm" | "anthropic") => Promise<void>;
  setGlmEndpoint: (endpoint: string) => Promise<void>;
  setRefreshSecs: (secs: number) => Promise<void>;
  setLiveClaude: (enabled: boolean) => Promise<void>;
  claudeSignedIn: boolean;
  claudeExpired: boolean;
  claudeSignOut: () => Promise<unknown>;
  claudeSignOutError: string | null;
  claudeLoginStart: () => Promise<ClaudeLoginInfo | null>;
  claudeLoginFinish: (code: string) => Promise<unknown>;
  claudeLoginCancel: () => void;
  claudeLoginBusy: boolean;
  claudeLoginError: string | null;
  setLaunchOnStartup: (enabled: boolean) => Promise<void>;
  setMinimalView: (enabled: boolean) => Promise<void>;
  setTooltipProvider: (provider: TooltipProvider) => Promise<void>;
  setWindowMode: (mode: WindowMode) => Promise<void>;
  copilotConnected: boolean;
  connectCopilotStart: () => Promise<CopilotDeviceCode | null>;
  copilotPoll: () => Promise<string | null>;
  copilotCancel: () => void;
  disconnectCopilot: () => Promise<void>;
  reloadSettings: () => Promise<void>;
  bailianStatus: () => Promise<BailianCliStatus | null>;
  installBailian: () => Promise<string | null>;
  bailianInstallBusy: boolean;
  bailianInstallError: string | null;
  loginBailian: () => Promise<string | null>;
  bailianLoginBusy: boolean;
  bailianLoginError: string | null;
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
  claudeSignedIn,
  claudeExpired,
  claudeSignOut,
  claudeSignOutError,
  claudeLoginStart,
  claudeLoginFinish,
  claudeLoginCancel,
  claudeLoginBusy,
  claudeLoginError,
  setLaunchOnStartup,
  setMinimalView,
  setTooltipProvider,
  setWindowMode,
  copilotConnected,
  connectCopilotStart,
  copilotPoll,
  copilotCancel,
  disconnectCopilot,
  reloadSettings,
  bailianStatus,
  installBailian,
  bailianInstallBusy,
  bailianInstallError,
  loginBailian,
  bailianLoginBusy,
  bailianLoginError,
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
          <option value="alibaba">Alibaba Cloud</option>
        </select>
      </div>
      <div className="key-row">
        <div className="key-top">
          <span className="key-label">Window mode</span>
        </div>
        <span className="connect-sub" style={{ margin: "0 0 6px" }}>
          Dock anchors the window to the tray icon. Float lets you drag it anywhere — including across monitors.
        </span>
        <select
          className="interval-select"
          value={settings.windowMode}
          onChange={(e) => setWindowMode(e.target.value as WindowMode)}
        >
          <option value="dock">Dock</option>
          <option value="float">Float</option>
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
        <span className="meta">
          {claudeSignedIn && !claudeExpired ? "connected" : "not connected"}
        </span>
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
      {claudeSignedIn && !claudeExpired ? (
        <ClaudeSignOut signOut={claudeSignOut} signOutError={claudeSignOutError} />
      ) : (
        <ClaudeSignIn
          expired={claudeExpired}
          start={claudeLoginStart}
          finish={claudeLoginFinish}
          cancel={claudeLoginCancel}
          busy={claudeLoginBusy}
          error={claudeLoginError}
        />
      )}
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
        <span className="meta">{copilotConnected ? "connected" : "not connected"}</span>
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

      <div className="sec-head">
        <h2>Alibaba Cloud</h2>
        <span className="meta">via Bailian CLI</span>
      </div>
      <BailianCli
        status={bailianStatus}
        install={installBailian}
        installBusy={bailianInstallBusy}
        installError={bailianInstallError}
        login={loginBailian}
        loginBusy={bailianLoginBusy}
        loginError={bailianLoginError}
      />

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

/** Sign in to Claude from Settings — the counterpart to ClaudeSignOut, so there's
 * a way back in right where you signed out (the Overview only shows a subtle
 * "not signed in" link, and only while live mode is on). Same copy-paste OAuth
 * flow as the Overview: open the browser, paste the CODE#STATE, finish. On
 * success the snapshot flips claudeSignedIn=true and this is replaced by the
 * sign-out row. The login is shared with the `claude` CLI, so this signs it in too. */
function ClaudeSignIn({
  expired,
  start,
  finish,
  cancel,
  busy,
  error,
}: {
  expired: boolean;
  start: () => Promise<ClaudeLoginInfo | null>;
  finish: (code: string) => Promise<unknown>;
  cancel: () => void;
  busy: boolean;
  error: string | null;
}) {
  const [awaiting, setAwaiting] = useState(false);
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const [code, setCode] = useState("");

  const begin = async () => {
    const info = await start();
    if (info) {
      setAuthUrl(info.authorizeUrl);
      setAwaiting(true);
    }
  };
  const submit = async () => {
    if (!code.trim() || busy) return;
    await finish(code.trim());
    // Success → snapshot sets claudeSignedIn=true → this unmounts.
  };
  const abort = () => {
    cancel();
    setAwaiting(false);
    setCode("");
  };

  return (
    <div className="key-row">
      <div className="key-top">
        <span className="key-label">Claude login</span>
        <span className="key-status">{expired ? "⚠ expired" : "○ not connected"}</span>
      </div>
      <span className="connect-sub" style={{ margin: "0 0 8px" }}>
        {expired
          ? "Your Claude login expired — reconnect to restore live usage."
          : "Connect your Claude Pro/Max account for live session & weekly usage."}{" "}
        Shares the Claude Code CLI login, so connecting signs <code>claude</code> in too.
      </span>
      {awaiting ? (
        <>
          <span className="connect-sub" style={{ margin: "0 0 6px" }}>
            Approve in your browser, then paste the code it shows you.{" "}
            {authUrl && (
              <a
                className="about-link"
                href="#"
                onClick={(e) => {
                  e.preventDefault();
                  void invoke("open_url", { url: authUrl });
                }}
              >
                Re-open page
              </a>
            )}
          </span>
          <div className="key-input">
            <input
              type="text"
              value={code}
              spellCheck={false}
              autoComplete="off"
              autoFocus
              placeholder="Paste code (looks like abc…#xyz…)"
              onChange={(e) => setCode(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void submit();
              }}
            />
          </div>
          <div style={{ display: "flex", gap: 6, marginTop: 8 }}>
            <button
              className="btn primary"
              disabled={busy || !code.trim()}
              onClick={() => void submit()}
            >
              {busy ? "Verifying…" : "Finish"}
            </button>
            <button className="btn" disabled={busy} onClick={abort}>
              Cancel
            </button>
          </div>
        </>
      ) : (
        <button className="btn primary" disabled={busy} onClick={() => void begin()}>
          {busy ? "Starting…" : expired ? "Reconnect Claude" : "Connect Claude"}
        </button>
      )}
      {error && <p className="key-err">{error}</p>}
    </div>
  );
}

/** Full Claude sign-out. The Claude login is the SHARED Claude Code credential
 * (not an app-only token like Copilot), so this signs the `claude` CLI out too —
 * hence the warning + an explicit confirm step before the destructive action. */
function ClaudeSignOut({
  signOut,
  signOutError,
}: {
  signOut: () => Promise<unknown>;
  signOutError: string | null;
}) {
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  // Local flag so the backend error only shows after an actual attempt (not a
  // stale error from an earlier session). The message itself comes from the
  // command's error, which carries the specific reason (which store is stuck).
  const [attempted, setAttempted] = useState(false);

  return (
    <div className="key-row">
      <div className="key-top">
        <span className="key-label">Claude login</span>
        <span className="key-status set">● connected</span>
      </div>
      <span className="connect-sub" style={{ margin: "0 0 8px" }}>
        Disconnecting removes the Claude Code login (shared with the CLI) — connect
        again here or with <code>claude /login</code>. The Claude Desktop app has its own
        separate login and isn’t affected; a running <code>claude</code> session keeps
        working until you restart it.
      </span>
      {confirm ? (
        <div style={{ display: "flex", gap: 6 }}>
          <button
            className="btn"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              setAttempted(true);
              const ok = await signOut();
              setBusy(false);
              // On success the snapshot clears claudeSignedIn and this unmounts;
              // on failure signOutError (from the command) holds the reason.
              if (ok) setConfirm(false);
            }}
          >
            {busy ? "Disconnecting…" : "Confirm disconnect"}
          </button>
          <button className="btn" disabled={busy} onClick={() => setConfirm(false)}>
            Cancel
          </button>
        </div>
      ) : (
        <button className="btn" onClick={() => setConfirm(true)}>
          Disconnect
        </button>
      )}
      {attempted && !busy && signOutError && <p className="key-err">{signOutError}</p>}
    </div>
  );
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

  // Shared status header so every state shows the same row as the Claude login
  // (connected / not connected), instead of only showing it when connected.
  const header = (
    <div className="key-top">
      <span className="key-label">Copilot login</span>
      <span className={`key-status ${connected ? "set" : ""}`}>
        {connected ? "● connected" : "○ not connected"}
      </span>
    </div>
  );

  if (connected) {
    return (
      <div className="key-row">
        {header}
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
        {header}
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
      {header}
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

function BailianCli({
  status,
  install,
  installBusy,
  installError,
  login,
  loginBusy,
  loginError,
}: {
  status: () => Promise<BailianCliStatus | null>;
  install: () => Promise<string | null>;
  installBusy: boolean;
  installError: string | null;
  login: () => Promise<string | null>;
  loginBusy: boolean;
  loginError: string | null;
}) {
  const [cli, setCli] = useState<BailianCliStatus | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [checking, setChecking] = useState(true);
  const statusRef = useRef(status);
  statusRef.current = status;

  useEffect(() => {
    (async () => {
      const s = await statusRef.current();
      setCli(s);
      setChecking(false);
    })();
  }, []);

  const doInstall = async () => {
    setMsg(null);
    const result = await install();
    if (result) {
      setMsg(result);
      const s = await status();
      setCli(s);
    }
  };

  const doLogin = async () => {
    setMsg(null);
    const result = await login();
    if (result) {
      setMsg(result);
      const s = await status();
      setCli(s);
    }
  };

  if (checking) {
    return (
      <div className="key-row">
        <span className="connect-sub">Checking for Bailian CLI…</span>
      </div>
    );
  }

  // Not installed → show install button.
  if (!cli?.installed) {
    return (
      <div className="key-row">
        <div className="key-top">
          <span className="key-label">Bailian CLI (<code>bl</code>)</span>
          <span className="key-status">○ not installed</span>
        </div>
        <span className="connect-sub" style={{ margin: "0 0 8px" }}>
          The Bailian CLI reads your Alibaba Cloud Model Studio usage. Requires
          Node.js ≥ 22.12 and npm.
        </span>
        <button className="btn primary" disabled={installBusy} onClick={() => void doInstall()}>
          {installBusy ? "Installing…" : "Install Bailian CLI"}
        </button>
        {msg && <span className="connect-sub" style={{ margin: "8px 0 0" }}>{msg}</span>}
        {installError && <p className="key-err">{installError}</p>}
      </div>
    );
  }

  // Installed but not authenticated → show login button.
  if (!cli.authenticated) {
    return (
      <div className="key-row">
        <div className="key-top">
          <span className="key-label">Bailian CLI (<code>bl</code>)</span>
          <span className="key-status">○ not authenticated</span>
        </div>
        <span className="connect-sub" style={{ margin: "0 0 8px" }}>
          Installed. Sign in to connect your Alibaba Cloud account — a browser
          window will open to complete the login.
        </span>
        <button className="btn primary" disabled={loginBusy} onClick={() => void doLogin()}>
          {loginBusy ? "Signing in…" : "Sign in to Alibaba Cloud"}
        </button>
        {msg && <span className="connect-sub" style={{ margin: "8px 0 0" }}>{msg}</span>}
        {loginError && <p className="key-err">{loginError}</p>}
      </div>
    );
  }

  // Installed + authenticated → show connected status.
  return (
    <div className="key-row">
      <div className="key-top">
        <span className="key-label">Bailian CLI (<code>bl</code>)</span>
        <span className="key-status set">● connected</span>
      </div>
      <span className="connect-sub" style={{ margin: "0 0 6px" }}>
        {cli.authHint ?? "Authenticated via Bailian CLI."}
      </span>
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
