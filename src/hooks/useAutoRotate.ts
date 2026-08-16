import { useEffect } from "react";

type ProviderTab = "claude" | "glm" | "copilot" | "alibaba" | "kimi" | "grok";

/**
 * Cycles through provider tabs on a timer while `enabled` is true.
 * The interval is torn down and rebuilt whenever the tab list or speed changes.
 */
export function useAutoRotate(
  enabled: boolean,
  intervalMs: number,
  tabs: ProviderTab[],
  setProvider: (updater: (prev: ProviderTab) => ProviderTab) => void,
) {
  const rotateKey = tabs.join(",");

  useEffect(() => {
    if (!enabled) return;
    const list = rotateKey.split(",") as ProviderTab[];
    if (list.length <= 1) return;
    const id = setInterval(() => {
      setProvider((prev) => {
        const idx = Math.max(0, list.indexOf(prev));
        return list[(idx + 1) % list.length];
      });
    }, intervalMs);
    return () => clearInterval(id);
  }, [enabled, intervalMs, rotateKey, setProvider]);
}
