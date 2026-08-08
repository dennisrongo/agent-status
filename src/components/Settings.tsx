import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import type { BailianCliStatus, ClaudeLoginInfo, CopilotDeviceCode, SettingsView, TooltipProvider, VendorStatus, WindowMode } from "../types";

/** Clickable info icon that opens a popover with help text. Stays open until
 * the user clicks outside — so they can follow multi-step instructions. */
function InfoTip({ children }: { children: React.ReactNode }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  return (
    <span className="info-wrap" ref={ref}>
      <button
        className={`info-icon${open ? " open" : ""}`}
        onClick={(e) => { e.preventDefault(); e.stopPropagation(); setOpen(!open); }}
        tabIndex={-1}
        aria-label="More info"
      >
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
          <circle cx="8" cy="8" r="6.5" />
          <path d="M8 7v3.5M8 5h.01" />
        </svg>
      </button>
      {open && <span className="info-pop">{children}</span>}
    </span>
  );
}

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
  setHiddenProviders: (providers: string[]) => Promise<void>;
  setAutoRotate: (enabled: boolean) => Promise<void>;
  setRotateSecs: (secs: number) => Promise<void>;
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
  logoutBailian: () => Promise<string | null>;
  bailianLogoutBusy: boolean;
  bailianLogoutError: string | null;
  setBailianOpenApi: (accessKeyId: string, accessKeySecret: string) => Promise<string | null>;
  bailianSetOpenApiBusy: boolean;
  bailianSetOpenApiError: string | null;
  /** Authoritative Alibaba status from the usage fetch — reflects the real
   * connection state (incl. a console session that `bl auth status` can't see
   * as expired). Falls back to `bailianStatus()` before the first snapshot. */
  alibabaVendorStatus?: VendorStatus;
  /** Authoritative Kimi Code status from the usage fetch — configured means the
   * CLI's OAuth login was found; authExpired means it's stale. */
  kimiVendorStatus?: VendorStatus;
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

const ROTATE_OPTIONS = [
  { secs: 10, label: "10 seconds" },
  { secs: 20, label: "20 seconds" },
  { secs: 30, label: "30 seconds" },
  { secs: 40, label: "40 seconds" },
  { secs: 50, label: "50 seconds" },
  { secs: 60, label: "60 seconds" },
];

const MAX_OVERVIEW = 5;

const OVERVIEW_PROVIDERS = [
  { id: "claude", label: "Claude" },
  { id: "glm", label: "GLM / z.ai" },
  { id: "copilot", label: "GitHub Copilot" },
  { id: "alibaba", label: "Alibaba Cloud" },
  { id: "kimi", label: "Kimi Code" },
] as const;

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
  setHiddenProviders,
  setAutoRotate,
  setRotateSecs,
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
  logoutBailian,
  bailianLogoutBusy,
  bailianLogoutError,
  setBailianOpenApi,
  bailianSetOpenApiBusy,
  bailianSetOpenApiError,
  alibabaVendorStatus,
  kimiVendorStatus,
  keyError,
}: Props) {
  const hidden = settings.hiddenProviders;
  const checkedCount = OVERVIEW_PROVIDERS.filter((p) => !hidden.includes(p.id)).length;
  const atCapacity = checkedCount >= MAX_OVERVIEW;

  const toggleOverview = (id: string, show: boolean) => {
    if (show && atCapacity) return;
    const next = show
      ? hidden.filter((p) => p !== id)
      : [...hidden, id];
    void setHiddenProviders(next);
  };

  const providerStatus = (id: string): string => {
    switch (id) {
      case "claude": return claudeSignedIn && !claudeExpired ? "connected" : "not connected";
      case "glm": return settings.glmKeySet ? "API key set" : "no API key";
      case "copilot": return copilotConnected ? "connected" : "not connected";
      case "alibaba": return alibabaVendorStatus?.configured ? "CLI configured" : "not configured";
      case "kimi": return kimiVendorStatus?.configured
        ? kimiVendorStatus.authExpired ? "login expired" : "connected"
        : "not detected";
      default: return "";
    }
  };

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
            <span className="key-label">Minimal view<InfoTip>Show only the headline stats on Overview and shrink the window to fit — no scrolling. Off shows the full breakdown.</InfoTip></span>
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
          <span className="key-label">Tray hover provider<InfoTip>Which provider's usage the menu-bar hover popover previews.</InfoTip></span>
        </div>
        <select
          className="interval-select"
          value={settings.tooltipProvider}
          onChange={(e) => setTooltipProvider(e.target.value as TooltipProvider)}
        >
          <option value="claude">Claude</option>
          <option value="glm">GLM / z.ai</option>
          <option value="copilot">GitHub Copilot</option>
          <option value="alibaba">Alibaba Cloud</option>
          <option value="kimi">Kimi Code</option>
        </select>
      </div>
      <div className="key-row">
        <div className="key-top">
          <span className="key-label">Window mode<InfoTip>Dock anchors the window to the tray icon. Float lets you drag it anywhere — including across monitors.</InfoTip></span>
        </div>
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
          value={snapToPreset(REFRESH_OPTIONS, settings.refreshSecs)}
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
        <h2>Auto-rotate providers</h2>
        <span className="meta">{settings.autoRotate ? `every ${settings.rotateSecs}s` : "off"}</span>
      </div>
      <div className="key-row">
        <label className="toggle-row">
          <span>
            <span className="key-label">Rotate provider tabs<InfoTip>Cycle through visible providers on the Overview automatically so you can glance at each one without switching tabs.</InfoTip></span>
          </span>
          <input
            type="checkbox"
            className="toggle"
            checked={settings.autoRotate}
            onChange={(e) => setAutoRotate(e.target.checked)}
          />
        </label>
      </div>
      {settings.autoRotate && (
        <div className="key-row">
          <div className="key-top">
            <span className="key-label">Rotate interval</span>
          </div>
          <select
            className="interval-select"
            value={snapToPreset(ROTATE_OPTIONS, settings.rotateSecs)}
            onChange={(e) => setRotateSecs(Number(e.target.value))}
          >
            {ROTATE_OPTIONS.map((o) => (
              <option key={o.secs} value={o.secs}>
                {o.label}
              </option>
            ))}
          </select>
        </div>
      )}

      <div className="sec-head">
        <h2>Startup</h2>
        <span className="meta">{settings.launchOnStartup ? "on" : "off"}</span>
      </div>
      <div className="key-row">
        <label className="toggle-row">
          <span>
            <span className="key-label">Launch at login<InfoTip>Start Agent Usage Monitor automatically when you log in.</InfoTip></span>
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
        <h2>Overview providers</h2>
        <span className="meta">{checkedCount}/{MAX_OVERVIEW} selected</span>
      </div>
      {OVERVIEW_PROVIDERS.map((p) => {
        const isChecked = !hidden.includes(p.id);
        const disabled = !isChecked && atCapacity;
        return (
          <div className="key-row" key={p.id}>
            <label className="toggle-row" style={disabled ? { opacity: 0.45 } : undefined}>
              <span>
                <span className="key-label">{p.label}</span>
                <span className="connect-sub" style={{ margin: "2px 0 0" }}>
                  {providerStatus(p.id)}
                </span>
              </span>
              <input
                type="checkbox"
                className="toggle"
                checked={isChecked}
                disabled={disabled}
                onChange={(e) => toggleOverview(p.id, e.target.checked)}
              />
            </label>
          </div>
        );
      })}
      {atCapacity && (
        <div className="key-row">
          <span className="connect-sub" style={{ margin: "0", color: "var(--faint)" }}>
            Uncheck a provider to make room.
          </span>
        </div>
      )}

      <div className="sec-head">
        <h2>Claude / Anthropic</h2>
        <span className="meta">
          {claudeSignedIn && !claudeExpired ? "connected" : "not connected"}
        </span>
      </div>
      <div className="key-row">
        <label className="toggle-row">
          <span>
            <span className="key-label">Live usage from Claude Code<InfoTip>Reads your Claude Code login to show real session/weekly %. Off = local token estimate.</InfoTip></span>
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
          <span className="key-label">Endpoint<InfoTip>Usage API endpoint — verify it for your account / region.</InfoTip></span>
        </div>
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
        logout={logoutBailian}
        logoutBusy={bailianLogoutBusy}
        logoutError={bailianLogoutError}
        setOpenApi={setBailianOpenApi}
        setOpenApiBusy={bailianSetOpenApiBusy}
        setOpenApiError={bailianSetOpenApiError}
        vendorStatus={alibabaVendorStatus}
      />

      <div className="sec-head">
        <h2>Kimi Code</h2>
        <span className="meta">via Kimi Code CLI</span>
      </div>
      <div className="key-row">
        <div className="key-top">
          <span className="key-label">Kimi Code login<InfoTip>
            <p style={{ margin: "0 0 8px" }}>Reads the OAuth login the <strong>Kimi Code CLI</strong> stores on this machine to show your real weekly &amp; 5-hour quota — nothing to configure here.</p>
            <div className="info-steps">
              <div className="info-step">
                <span className="info-step-num">1</span>
                <span className="info-step-body">Install Kimi Code and sign in with <code>kimi login</code>.</span>
              </div>
              <div className="info-step">
                <span className="info-step-num">2</span>
                <span className="info-step-body">This app picks up the login automatically on the next refresh.</span>
              </div>
            </div>
            <p style={{ margin: "8px 0 0", fontSize: "10.5px", color: "var(--faint)" }}>
              The token is read-only here — an expired login is renewed by opening Kimi Code or running <code>kimi login</code> again.
            </p>
          </InfoTip></span>
          <span className={`key-status ${kimiVendorStatus?.configured && !kimiVendorStatus.authExpired ? "set" : ""}`}>
            {kimiVendorStatus?.configured
              ? kimiVendorStatus.authExpired
                ? "⚠ login expired"
                : "● connected"
              : "○ not detected"}
          </span>
        </div>
        <span className="connect-sub" style={{ margin: "0" }}>
          {kimiVendorStatus?.configured
            ? kimiVendorStatus.authExpired
              ? "Open Kimi Code or run kimi login to refresh the expired login."
              : kimiVendorStatus.ok
                ? kimiVendorStatus.secondary
                : (kimiVendorStatus.error ?? "reading usage…")
            : "Sign in to the Kimi Code CLI (kimi login) — the login is picked up automatically."}
        </span>
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
function snapToPreset(options: { secs: number }[], secs: number): number {
  return options.reduce((best, o) =>
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
        <span className="key-label">Claude login<InfoTip>
          {expired
            ? <p style={{ margin: "0 0 8px" }}>Your Claude login expired — reconnect to restore live usage.</p>
            : <p style={{ margin: "0 0 8px" }}>Connect your <strong>Claude Pro/Max</strong> account for live session &amp; weekly usage.</p>}
          <div className="info-steps">
            <div className="info-step">
              <span className="info-step-num">1</span>
              <span className="info-step-body">Click <strong>Connect Claude</strong> — a browser window opens to Anthropic.</span>
            </div>
            <div className="info-step">
              <span className="info-step-num">2</span>
              <span className="info-step-body">Approve the authorization in your browser.</span>
            </div>
            <div className="info-step">
              <span className="info-step-num">3</span>
              <span className="info-step-body">Paste the code shown back here and click <strong>Finish</strong>.</span>
            </div>
          </div>
          <p style={{ margin: "8px 0 0", fontSize: "10.5px", color: "var(--faint)" }}>
            Shares the <code>claude</code> CLI login — connecting signs it in too.
          </p>
        </InfoTip></span>
        <span className="key-status">{expired ? "⚠ expired" : "○ not connected"}</span>
      </div>
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
        <span className="key-label">Claude login<InfoTip>
          <p style={{ margin: "0 0 8px" }}>Disconnecting removes the <strong>shared Claude Code login</strong>. What happens:</p>
          <div className="info-steps">
            <div className="info-step">
              <span className="info-step-num" style={{ background: "color-mix(in oklch, var(--danger) 15%, var(--surface-2))", borderColor: "color-mix(in oklch, var(--danger) 30%, var(--border-2))", color: "var(--danger)" }}>!</span>
              <span className="info-step-body">The <code>claude</code> CLI is signed out — reconnect here or with <code>claude /login</code>.</span>
            </div>
            <div className="info-step">
              <span className="info-step-num" style={{ color: "var(--ok)", background: "color-mix(in oklch, var(--ok) 12%, var(--surface-2))", borderColor: "color-mix(in oklch, var(--ok) 25%, var(--border-2))" }}>✓</span>
              <span className="info-step-body"><strong>Claude Desktop</strong> app is not affected — it has its own separate login.</span>
            </div>
            <div className="info-step">
              <span className="info-step-num" style={{ color: "var(--ok)", background: "color-mix(in oklch, var(--ok) 12%, var(--surface-2))", borderColor: "color-mix(in oklch, var(--ok) 25%, var(--border-2))" }}>✓</span>
              <span className="info-step-body">A running <code>claude</code> session keeps working until you restart it.</span>
            </div>
          </div>
        </InfoTip></span>
        <span className="key-status set">● connected</span>
      </div>
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
        <span className="key-label">{label}{sub && <InfoTip>{sub}</InfoTip>}</span>
        <span className={`key-status ${isSet ? "set" : ""}`}>
          {isSet ? "● set" : "○ not set"}
        </span>
      </div>
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
      <span className="key-label">Copilot login<InfoTip>Usage is read automatically from your editor or <code>gh</code> CLI Copilot token. Only connect here if no token is found automatically. Authorizes via GitHub's device flow using VS Code Copilot's client ID.</InfoTip></span>
      <span className={`key-status ${connected ? "set" : ""}`}>
        {connected ? "● connected" : "○ not connected"}
      </span>
    </div>
  );

  if (connected) {
    return (
      <div className="key-row">
        {header}
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
      <button className="btn primary" disabled={busy} onClick={begin}>
        {busy ? "Starting…" : "Connect GitHub Copilot"}
      </button>
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
  logout,
  logoutBusy,
  logoutError,
  setOpenApi,
  setOpenApiBusy,
  setOpenApiError,
  vendorStatus,
}: {
  status: () => Promise<BailianCliStatus | null>;
  install: () => Promise<string | null>;
  installBusy: boolean;
  installError: string | null;
  login: () => Promise<string | null>;
  loginBusy: boolean;
  loginError: string | null;
  logout: () => Promise<string | null>;
  logoutBusy: boolean;
  logoutError: string | null;
  setOpenApi: (id: string, secret: string) => Promise<string | null>;
  setOpenApiBusy: boolean;
  setOpenApiError: string | null;
  vendorStatus?: VendorStatus;
}) {
  const [cli, setCli] = useState<BailianCliStatus | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [checking, setChecking] = useState(true);
  const [confirmLogout, setConfirmLogout] = useState(false);
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

  const doLogout = async () => {
    setMsg(null);
    const result = await logout();
    if (result) {
      setMsg(result);
      const s = await status();
      setCli(s);
    }
  };

  const doSetOpenApi = async (id: string, secret: string) => {
    setMsg(null);
    const result = await setOpenApi(id, secret);
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

  // The snapshot (from a usage fetch) is the authority for connected/expired —
  // `bl auth status` says `authenticated: true` as long as any credential is
  // present, which hides a stale console session. Fall back to it only before
  // the first collect arrives.
  const expired = vendorStatus?.authExpired ?? false;
  const connected = vendorStatus
    ? vendorStatus.ok && !expired
    : (cli?.authenticated ?? false);

  // Not installed → show install button.
  if (!cli?.installed) {
    return (
      <div className="key-row">
        <div className="key-top">
          <span className="key-label">Bailian CLI (<code>bl</code>)<InfoTip>Reads your Alibaba Cloud Model Studio usage. Requires Node.js ≥ 22.12 and npm.</InfoTip></span>
          <span className="key-status">○ not installed</span>
        </div>
        <button className="btn primary" disabled={installBusy} onClick={() => void doInstall()}>
          {installBusy ? "Installing…" : "Install Bailian CLI"}
        </button>
        {msg && <span className="connect-sub" style={{ margin: "8px 0 0" }}>{msg}</span>}
        {installError && <p className="key-err">{installError}</p>}
      </div>
    );
  }

  // Installed but not usable: either no credential at all, or the console
  // session expired (which `bl auth status` can't see). Both need the same fix
  // — `bl auth login --console` — so they share the sign-in affordance.
  if (!connected) {
    return (
      <div className="key-row">
        <div className="key-top">
          <span className="key-label">Bailian CLI (<code>bl</code>)<InfoTip>{expired
            ? "Your console session has expired. Sign in again to refresh it — a browser window will open."
            : "Sign in to connect your Alibaba Cloud account — a browser window will open to complete the login."}</InfoTip></span>
          <span className="key-status">{expired ? "○ session expired" : "○ not authenticated"}</span>
        </div>
        <button className="btn primary" disabled={loginBusy} onClick={() => void doLogin()}>
          {loginBusy ? "Signing in…" : "Sign in to Alibaba Cloud"}
        </button>
        {expired && !cli?.hasOpenApi && (
          <AkSkForm onSubmit={doSetOpenApi} busy={setOpenApiBusy} error={setOpenApiError} />
        )}
        {msg && <span className="connect-sub" style={{ margin: "8px 0 0" }}>{msg}</span>}
        {loginError && <p className="key-err">{loginError}</p>}
      </div>
    );
  }

  // Installed + authenticated → show connected status.
  return (
    <div className="key-row">
      <div className="key-top">
        <span className="key-label">Bailian CLI (<code>bl</code>)<InfoTip><AkSkStepsContent /></InfoTip></span>
        <span className="key-status set">● connected</span>
      </div>
      <span className="connect-sub" style={{ margin: "0 0 6px" }}>
        {cli?.authHint ?? "Authenticated via Bailian CLI."}
      </span>
      {!cli?.hasOpenApi ? (
        <AkSkForm onSubmit={doSetOpenApi} busy={setOpenApiBusy} error={setOpenApiError} />
      ) : (
        <span className="connect-sub" style={{ margin: "4px 0 0", color: "var(--faint)" }}>
          Auto-refresh enabled — the CLI will keep your session alive.
        </span>
      )}
      <div style={{ marginTop: 8 }}>
        {confirmLogout ? (
          <div style={{ display: "flex", gap: 6 }}>
            <button
              className="btn"
              disabled={logoutBusy}
              onClick={async () => {
                await doLogout();
                setConfirmLogout(false);
              }}
            >
              {logoutBusy ? "Disconnecting…" : "Confirm disconnect"}
            </button>
            <button className="btn" disabled={logoutBusy} onClick={() => setConfirmLogout(false)}>
              Cancel
            </button>
          </div>
        ) : (
          <button className="btn" onClick={() => setConfirmLogout(true)}>
            Disconnect
          </button>
        )}
        {logoutError && <p className="key-err">{logoutError}</p>}
      </div>
    </div>
  );
}

function AkSkStepsContent() {
  return (
    <div className="info-steps">
      <div className="info-step">
        <span className="info-step-num">1</span>
        <span className="info-step-body">
          <a href="#" onClick={(e) => { e.preventDefault(); void invoke("open_url", { url: "https://ram.console.aliyun.com/users" }); }}>
            Open RAM console
          </a>{" "}
          → <strong>Create User</strong> → check <strong>Permanent AccessKey</strong> → copy the ID &amp; Secret shown.
        </span>
      </div>
      <div className="info-step">
        <span className="info-step-num">2</span>
        <span className="info-step-body">
          On the user's <strong>Permissions</strong> tab → <strong>Grant Permission</strong> → attach both policies:<br />
          <code>AliyunModelStudioFullAccess</code><br />
          <code>AliyunBailianFullAccess</code>
        </span>
      </div>
      <div className="info-step">
        <span className="info-step-num">3</span>
        <span className="info-step-body">
          Paste the keys below and click <strong>Enable auto-refresh</strong>.
        </span>
      </div>
    </div>
  );
}

function AkSkForm({ onSubmit, busy, error }: {
  onSubmit: (id: string, secret: string) => Promise<void>;
  busy: boolean;
  error: string | null;
}) {
  const [akId, setAkId] = useState("");
  const [akSecret, setAkSecret] = useState("");
  const canSave = akId.trim().length > 0 && akSecret.trim().length > 0 && !busy;

  return (
    <div style={{ margin: "8px 0 0" }}>
      <div className="key-input" style={{ marginBottom: 4 }}>
        <input
          type="text"
          placeholder="AccessKey ID (LTAI5t…)"
          value={akId}
          onChange={(e) => setAkId(e.target.value)}
          spellCheck={false}
          autoComplete="off"
        />
      </div>
      <div className="key-input" style={{ marginBottom: 6 }}>
        <input
          type="password"
          placeholder="AccessKey Secret"
          value={akSecret}
          onChange={(e) => setAkSecret(e.target.value)}
          autoComplete="off"
        />
      </div>
      <button
        className="btn primary"
        disabled={!canSave}
        onClick={() => void onSubmit(akId.trim(), akSecret.trim())}
      >
        {busy ? "Saving…" : "Enable auto-refresh"}
      </button>
      {error && <p className="key-err">{error}</p>}
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
