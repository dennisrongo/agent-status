import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";

import { useUpdater } from "../hooks/useUpdater";
import { isTauriReady } from "../tauriReady";

const WEBSITE = "https://dennisrongo.com";

export function About() {
  const [version, setVersion] = useState<string | null>(null);
  const { phase, version: newVersion, error, check, install } = useUpdater({ auto: false });

  useEffect(() => {
    getVersion().then(setVersion).catch(() => setVersion(null));
  }, []);

  const busy = phase === "checking" || phase === "downloading" || phase === "ready";
  const showInstall = phase === "available" || phase === "downloading" || phase === "ready";
  const installLabel =
    phase === "downloading" ? "Downloading…" : phase === "ready" ? "Restarting…" : "Update & restart";
  const status =
    phase === "checking"
      ? "Checking…"
      : phase === "uptodate"
        ? "You’re on the latest version."
        : phase === "available"
          ? `Update available: v${newVersion}`
          : phase === "downloading"
            ? "Downloading…"
            : phase === "ready"
              ? "Restarting…"
              : phase === "error"
                ? `Couldn’t check${error ? ` — ${error}` : ""}`
                : "Check for a newer signed build.";

  const openWebsite = async (e: React.MouseEvent) => {
    e.preventDefault();
    if (!isTauriReady()) return;
    try {
      await invoke("open_url", { url: WEBSITE });
    } catch (err) {
      console.error("Failed to open website:", err);
    }
  };

  return (
    <section className="panel">
      <div className="about-hero">
        <span className="about-logo">
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
          <div className="about-name">Agent Usage Monitor</div>
          <div className="about-tagline">Menubar usage for AI coding agents</div>
        </div>
      </div>

      <div className="about-row">
        <span className="about-row-label">Version</span>
        <span className="about-value">{version ? `v${version}` : "—"}</span>
      </div>

      <div className="about-row">
        <div>
          <span className="about-row-label">Software update</span>
          <span className={`about-row-sub${phase === "error" ? " err" : ""}`}>{status}</span>
        </div>
        {showInstall ? (
          <button className="btn primary" onClick={install} disabled={busy}>
            {installLabel}
          </button>
        ) : (
          <button className="btn" onClick={check} disabled={busy}>
            {phase === "checking" ? "Checking…" : "Check for updates"}
          </button>
        )}
      </div>

      <div className="about-row">
        <div>
          <span className="about-row-label">Created by</span>
          <span className="about-row-sub">Dennis Rongo</span>
        </div>
        <a className="about-link" href={WEBSITE} onClick={openWebsite}>
          dennisrongo.com
          <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round">
            <path d="M4.5 2.5h5v5M9.5 2.5L2.5 9.5" />
          </svg>
        </a>
      </div>

      <div className="version-foot">
        Agent Usage Monitor{version ? ` v${version}` : ""} · © 2026 Lean Code Automation LLC
      </div>
    </section>
  );
}
