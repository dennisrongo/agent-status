import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { tileLabel } from "../format";
import { fitWindowHeight } from "../platform";
import { isTauriReady } from "../tauriReady";
import type { TooltipProvider, UsageSnapshot, VendorStatus } from "../types";

// Must match HOVER_WIDTH in tray.rs — Rust anchors the window's right edge to
// the tray icon, so the width has to stay fixed while we fit the height here.
const WIDTH = 300;

/**
 * Compact preview of one provider's usage, shown in its own borderless window
 * when the cursor hovers the tray icon. Listens for the same `usage-updated`
 * broadcast the main window uses, plus a `hover-provider` event that tells it
 * which provider (Claude, GLM, or Copilot) to show. Fits its window to the rendered
 * content so spacing is controlled by CSS rather than the OS tooltip.
 */
export function HoverPopover() {
  const [snapshot, setSnapshot] = useState<UsageSnapshot | null>(null);
  const [provider, setProvider] = useState<TooltipProvider>("claude");
  const lastGenMs = useRef(-1);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isTauriReady()) return;
    const unlisteners: (() => void)[] = [];
    (async () => {
      unlisteners.push(
        await listen<UsageSnapshot>("usage-updated", (e) => {
          // Drop out-of-order deliveries (several emitters can race), same guard
          // the main window uses.
          const gen = e.payload.meta.generatedMs ?? 0;
          if (gen < lastGenMs.current) return;
          lastGenMs.current = gen;
          setSnapshot(e.payload);
        }),
      );
      unlisteners.push(
        await listen<TooltipProvider>("hover-provider", (e) => {
          if (e.payload === "claude" || e.payload === "glm" || e.payload === "copilot")
            setProvider(e.payload);
        }),
      );
    })();
    return () => {
      for (const u of unlisteners) u();
    };
  }, []);

  // Fit the window to the card's natural height (it varies by provider and
  // state), so there's never dead space below the content. On macOS the window
  // is top-anchored under the icon and grows downward; on Windows fitWindowHeight
  // keeps the bottom edge pinned above the taskbar so it grows upward.
  useLayoutEffect(() => {
    if (!isTauriReady() || !rootRef.current) return;
    const height = Math.max(60, Math.ceil(rootRef.current.offsetHeight));
    fitWindowHeight(getCurrentWindow(), WIDTH, height).catch(() => {});
  }, [snapshot, provider]);

  return (
    <div className="hover-pop" ref={rootRef}>
      <HoverContent snapshot={snapshot} provider={provider} />
    </div>
  );
}

function HoverContent({
  snapshot,
  provider,
}: {
  snapshot: UsageSnapshot | null;
  provider: TooltipProvider;
}) {
  if (!snapshot) {
    return <div className="hp-status">Reading usage…</div>;
  }
  if (provider === "glm")
    return (
      <VendorMeters
        vendor={snapshot.vendor?.glm}
        srcLabel="z.ai"
        setupHint="Add a GLM API key in the app to see quota."
        errorLead="Couldn’t reach z.ai"
      />
    );
  if (provider === "copilot")
    return (
      <VendorMeters
        vendor={snapshot.vendor?.copilot}
        srcLabel="Copilot"
        setupHint="Connect GitHub Copilot in the app to see quota."
        errorLead="Couldn’t read Copilot usage"
      />
    );
  return <ClaudeContent snapshot={snapshot} />;
}

function ClaudeContent({ snapshot }: { snapshot: UsageSnapshot }) {
  const { limits, detection } = snapshot;

  // A present, non-expired Claude login is required to show any Claude usage
  // (the local estimate included) — without it, show only a connect / reconnect
  // line, never the stats, mirroring the main window. Default to "connected" if
  // a snapshot ever lacks detection so a valid reading isn't blanked.
  const claudeConnected = detection
    ? detection.claudeSignedIn && !detection.claudeExpired
    : true;
  if (!claudeConnected) {
    const expired = detection?.claudeExpired ?? false;
    return (
      <>
        <Head src="Claude" />
        <div className={`hp-status${expired ? " warn" : ""}`}>
          {expired
            ? "Claude login expired — reconnect in the app."
            : "Connect Claude in the app to see usage."}
        </div>
      </>
    );
  }

  const source = limits.live ? "live · Claude" : `${limits.planLabel} plan · est.`;
  return (
    <>
      <Head src={source} />
      {limits.buckets.length > 0 ? (
        <div className="hp-rows">
          {limits.buckets.slice(0, 3).map((b) => (
            <MeterRow
              key={b.name}
              status={b.status}
              label={tileLabel(b.name)}
              aux={`resets ${b.reset}`}
              pct={b.usedPct}
            />
          ))}
        </div>
      ) : (
        <div className="hp-status">
          {limits.pending ? "Reading live Claude usage…" : "No usage data yet."}
        </div>
      )}
    </>
  );
}

/**
 * One quota meter: a status dot, label, faint right-aligned aux text, the
 * percent, and a progress bar below. Shared by Claude buckets and GLM quota
 * windows so both providers render identically.
 */
function MeterRow({
  status,
  label,
  aux,
  pct,
}: {
  status: "ok" | "warn" | "danger";
  label: string;
  aux: string;
  pct: number;
}) {
  return (
    <div className="hp-row">
      <div className="hp-line">
        <span className={`hp-dot ${status}`} />
        <span className="hp-label">{label}</span>
        <span className="hp-reset">{aux}</span>
        <span className="hp-pct">{pct}%</span>
      </div>
      <div className="track">
        <div className={`fill ${status}`} style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

/**
 * Live usage for an API-key vendor (GLM, Copilot). Each quota window renders as
 * a MeterRow — same status dot + bar as Claude's buckets — while non-metered
 * facts (plan, reset date) fall back to a plain key/value line.
 */
function VendorMeters({
  vendor,
  srcLabel,
  setupHint,
  errorLead,
}: {
  vendor: VendorStatus | undefined;
  srcLabel: string;
  setupHint: string;
  errorLead: string;
}) {
  const live = Boolean(vendor?.configured && vendor.ok);

  return (
    <>
      <Head src={live ? `live · ${srcLabel}` : srcLabel} />
      {!vendor || !vendor.configured ? (
        <div className="hp-status">{setupHint}</div>
      ) : !vendor.ok ? (
        <div className="hp-status warn">
          {errorLead}
          {vendor.error ? `: ${vendor.error}` : ""}.
        </div>
      ) : (
        <div className="hp-rows">
          {vendor.detail.map((d, i) =>
            d.pct != null ? (
              <MeterRow
                key={`${d.label}-${i}`}
                status={d.status ?? "ok"}
                label={d.label}
                aux={d.value}
                pct={d.pct}
              />
            ) : (
              <div className="hp-kv" key={`${d.label}-${i}`}>
                <span className="hp-kv-label">{d.label}</span>
                <span className="hp-kv-val">{d.value}</span>
              </div>
            ),
          )}
        </div>
      )}
    </>
  );
}

function Head({ src }: { src: string }) {
  return (
    <div className="hp-head">
      <span className="hp-title">Agent Usage</span>
      <span className="hp-src">{src}</span>
    </div>
  );
}
