import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";

import { tileLabel } from "../format";
import { isTauriReady } from "../tauriReady";
import type { TooltipProvider, UsageSnapshot, VendorStatus } from "../types";

// Must match HOVER_WIDTH in tray.rs — Rust anchors the window's right edge to
// the tray icon, so the width has to stay fixed while we fit the height here.
const WIDTH = 300;

/**
 * Compact preview of one provider's usage, shown in its own borderless window
 * when the cursor hovers the tray icon. Listens for the same `usage-updated`
 * broadcast the main window uses, plus a `hover-provider` event that tells it
 * which provider (Claude or GLM) to show. Fits its window to the rendered
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
          if (e.payload === "claude" || e.payload === "glm") setProvider(e.payload);
        }),
      );
    })();
    return () => {
      for (const u of unlisteners) u();
    };
  }, []);

  // Fit the window to the card's natural height (it varies by provider and
  // state), so there's never dead space below the content. The window is
  // top-anchored under the icon, so it grows downward.
  useLayoutEffect(() => {
    if (!isTauriReady() || !rootRef.current) return;
    const height = Math.max(60, Math.ceil(rootRef.current.offsetHeight));
    getCurrentWindow()
      .setSize(new LogicalSize(WIDTH, height))
      .catch(() => {});
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
  return provider === "glm" ? (
    <GlmContent vendor={snapshot.vendor?.glm} />
  ) : (
    <ClaudeContent snapshot={snapshot} />
  );
}

function ClaudeContent({ snapshot }: { snapshot: UsageSnapshot }) {
  const { limits } = snapshot;
  const source = limits.live ? "live · Claude" : `${limits.planLabel} plan · est.`;

  return (
    <>
      <Head src={source} />
      {limits.needsReauth ? (
        <div className="hp-status warn">
          Claude login expired — open the app to reconnect.
        </div>
      ) : limits.buckets.length === 0 ? (
        <div className="hp-status">
          {limits.pending ? "Reading live Claude usage…" : "No usage data yet."}
        </div>
      ) : (
        <div className="hp-rows">
          {limits.buckets.slice(0, 3).map((b) => (
            <div className="hp-row" key={b.name}>
              <div className="hp-line">
                <span className={`hp-dot ${b.status}`} />
                <span className="hp-label">{tileLabel(b.name)}</span>
                <span className="hp-reset">resets {b.reset}</span>
                <span className="hp-pct">{b.usedPct}%</span>
              </div>
              <div className="track">
                <div
                  className={`fill ${b.status}`}
                  style={{ width: `${b.usedPct}%` }}
                />
              </div>
            </div>
          ))}
        </div>
      )}
    </>
  );
}

function GlmContent({ vendor }: { vendor: VendorStatus | undefined }) {
  const live = Boolean(vendor?.configured && vendor.ok);

  return (
    <>
      <Head src={live ? "live · z.ai" : "z.ai"} />
      {!vendor || !vendor.configured ? (
        <div className="hp-status">Add a GLM API key in the app to see quota.</div>
      ) : !vendor.ok ? (
        <div className="hp-status warn">
          Couldn’t reach z.ai{vendor.error ? `: ${vendor.error}` : ""}.
        </div>
      ) : (
        <div className="hp-rows">
          <div className="hp-glm-head">
            <span className="hp-label">{vendor.secondary || "balance"}</span>
            <span className="hp-pct">{vendor.primary}</span>
          </div>
          {vendor.detail.map((d) => (
            <div className="hp-kv" key={d.label}>
              <span className="hp-kv-label">{d.label}</span>
              <span className="hp-kv-val">{d.value}</span>
            </div>
          ))}
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
