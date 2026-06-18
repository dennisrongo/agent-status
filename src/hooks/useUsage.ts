import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { isTauriReady } from "../tauriReady";
import type { PlanKey, SettingsView, UsageSnapshot } from "../types";
import { useTauriCommand } from "./useTauriCommand";

type Provider = "glm" | "anthropic";

/**
 * Loads the usage snapshot, subscribes to background `usage-updated` events,
 * and exposes refresh + plan/endpoint/key mutations.
 */
export function useUsage() {
  const usageCmd = useTauriCommand<UsageSnapshot>("get_usage");
  const settingsCmd = useTauriCommand<SettingsView>("get_settings");
  const planCmd = useTauriCommand<SettingsView>("set_plan");
  const refreshSecsCmd = useTauriCommand<SettingsView>("set_refresh_secs");
  const liveClaudeCmd = useTauriCommand<SettingsView>("set_live_claude");
  const endpointCmd = useTauriCommand<SettingsView>("set_glm_endpoint");
  const setKeyCmd = useTauriCommand<SettingsView>("set_api_key");
  const clearKeyCmd = useTauriCommand<SettingsView>("clear_api_key");

  const [snapshot, setSnapshot] = useState<UsageSnapshot | null>(null);
  const [settings, setSettings] = useState<SettingsView | null>(null);

  // Several sources push snapshots (the command return, the `usage-updated`
  // event, the background loop, refresh-on-open). Apply one only if it's at
  // least as fresh as what's displayed, so an out-of-order delivery can't make
  // the UI flip back to older data.
  const lastGenMs = useRef(-1);
  const applySnapshot = useCallback((data: UsageSnapshot) => {
    const gen = data.meta.generatedMs ?? 0;
    if (gen < lastGenMs.current) return;
    lastGenMs.current = gen;
    setSnapshot(data);
  }, []);

  const refresh = useCallback(async () => {
    const data = await usageCmd.execute();
    if (data) applySnapshot(data);
  }, [usageCmd, applySnapshot]);

  const setPlan = useCallback(
    async (plan: PlanKey) => {
      const updated = await planCmd.execute({ plan });
      if (updated) {
        setSettings(updated);
        await refresh();
      }
    },
    [planCmd, refresh],
  );

  const setRefreshSecs = useCallback(
    async (secs: number) => {
      const updated = await refreshSecsCmd.execute({ secs });
      if (updated) setSettings(updated);
    },
    [refreshSecsCmd],
  );

  const setLiveClaude = useCallback(
    async (enabled: boolean) => {
      const updated = await liveClaudeCmd.execute({ enabled });
      if (updated) {
        setSettings(updated);
        await refresh();
      }
    },
    [liveClaudeCmd, refresh],
  );

  const setGlmEndpoint = useCallback(
    async (endpoint: string) => {
      const updated = await endpointCmd.execute({ endpoint });
      if (updated) setSettings(updated);
    },
    [endpointCmd],
  );

  const setApiKey = useCallback(
    async (provider: Provider, key: string) => {
      const updated = await setKeyCmd.execute({ provider, key });
      if (updated) {
        setSettings(updated);
        await refresh();
      }
      return updated;
    },
    [setKeyCmd, refresh],
  );

  const clearApiKey = useCallback(
    async (provider: Provider) => {
      const updated = await clearKeyCmd.execute({ provider });
      if (updated) {
        setSettings(updated);
        await refresh();
      }
    },
    [clearKeyCmd, refresh],
  );

  useEffect(() => {
    if (!isTauriReady()) return;
    let unlisten: (() => void) | undefined;

    (async () => {
      const view = await settingsCmd.execute();
      if (view) setSettings(view);
      await refresh();
      unlisten = await listen<UsageSnapshot>("usage-updated", (e) => {
        applySnapshot(e.payload);
      });
    })();

    return () => {
      if (unlisten) unlisten();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return {
    snapshot,
    settings,
    setPlan,
    setRefreshSecs,
    setLiveClaude,
    setGlmEndpoint,
    setApiKey,
    clearApiKey,
    refresh,
    isLoading: usageCmd.isLoading,
    error: usageCmd.error,
    keyError: setKeyCmd.error,
  };
}
