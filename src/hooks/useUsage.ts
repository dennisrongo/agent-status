import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { isTauriReady } from "../tauriReady";
import type {
  BailianCliStatus,
  ClaudeLoginInfo,
  CopilotDeviceCode,
  CodexCliStatus,
  CodexLoginInfo,
  GrokCliStatus,
  KimiCliStatus,
  PlanKey,
  SettingsView,
  TooltipProvider,
  UsageSnapshot,
  WindowMode,
} from "../types";
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
  const launchOnStartupCmd = useTauriCommand<SettingsView>("set_launch_on_startup");
  const minimalViewCmd = useTauriCommand<SettingsView>("set_minimal_view");
  const tooltipProviderCmd = useTauriCommand<SettingsView>("set_tooltip_provider");
  const windowModeCmd = useTauriCommand<SettingsView>("set_window_mode");
  const hiddenProvidersCmd = useTauriCommand<SettingsView>("set_hidden_providers");
  const autoRotateCmd = useTauriCommand<SettingsView>("set_auto_rotate");
  const rotateSecsCmd = useTauriCommand<SettingsView>("set_rotate_secs");
  const endpointCmd = useTauriCommand<SettingsView>("set_glm_endpoint");
  const setKeyCmd = useTauriCommand<SettingsView>("set_api_key");
  const clearKeyCmd = useTauriCommand<SettingsView>("clear_api_key");
  const copilotStartCmd = useTauriCommand<CopilotDeviceCode>("copilot_device_start");
  const copilotPollCmd = useTauriCommand<string>("copilot_device_poll");
  const copilotCancelCmd = useTauriCommand<void>("copilot_device_cancel");
  const disconnectCopilotCmd = useTauriCommand<SettingsView>("disconnect_copilot");
  const claudeLoginStartCmd = useTauriCommand<ClaudeLoginInfo>("claude_login_start");
  const claudeLoginFinishCmd = useTauriCommand<UsageSnapshot>("claude_login_finish");
  const claudeLoginCancelCmd = useTauriCommand<void>("claude_login_cancel");
  const claudeSignOutCmd = useTauriCommand<UsageSnapshot>("claude_sign_out");
  const bailianStatusCmd = useTauriCommand<BailianCliStatus>("bailian_cli_status");
  const bailianInstallCmd = useTauriCommand<string>("install_bailian_cli");
  const bailianLoginCmd = useTauriCommand<string>("bailian_cli_login");
  const bailianLogoutCmd = useTauriCommand<string>("bailian_cli_logout");
  const bailianSetOpenApiCmd = useTauriCommand<string>("bailian_set_open_api");
  const kimiStatusCmd = useTauriCommand<KimiCliStatus>("kimi_cli_status");
  const kimiLoginCmd = useTauriCommand<string>("kimi_cli_login");
  const kimiLogoutCmd = useTauriCommand<string>("kimi_cli_logout");
  const grokStatusCmd = useTauriCommand<GrokCliStatus>("grok_cli_status");
  const grokInstallCmd = useTauriCommand<string>("install_grok_cli");
  const grokLoginCmd = useTauriCommand<string>("grok_cli_login");
  const grokLogoutCmd = useTauriCommand<string>("grok_cli_logout");
  const codexStatusCmd = useTauriCommand<CodexCliStatus>("codex_cli_status");
  const codexInstallCmd = useTauriCommand<string>("install_codex_cli");
  const codexLoginStartCmd = useTauriCommand<CodexLoginInfo>("codex_login_start");
  const codexLoginCancelCmd = useTauriCommand<void>("codex_login_cancel");
  const codexLogoutCmd = useTauriCommand<string>("codex_cli_logout");

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

  // Full in-app Claude OAuth login (copy-paste). `start` opens the browser and
  // returns the authorize URL; `finish` exchanges the pasted CODE#STATE and
  // returns the refreshed snapshot; `cancel` drops the pending PKCE secrets.
  const claudeLoginStart = useCallback(
    () => claudeLoginStartCmd.execute(),
    [claudeLoginStartCmd],
  );
  const claudeLoginFinish = useCallback(
    async (code: string) => {
      const data = await claudeLoginFinishCmd.execute({ code });
      if (data) applySnapshot(data);
      return data;
    },
    [claudeLoginFinishCmd, applySnapshot],
  );
  const claudeLoginCancel = useCallback(() => {
    void claudeLoginCancelCmd.execute();
  }, [claudeLoginCancelCmd]);

  // Full sign-out: deletes the shared Claude Code credential (logs the CLI out
  // too) and re-pulls usage so the UI drops to the signed-out / estimate state.
  // Returns the snapshot (null on failure) so the caller can show an error.
  const claudeSignOut = useCallback(async () => {
    const data = await claudeSignOutCmd.execute();
    if (data) applySnapshot(data);
    return data;
  }, [claudeSignOutCmd, applySnapshot]);

  const bailianStatus = useCallback(
    () => bailianStatusCmd.execute(),
    [bailianStatusCmd],
  );

  const installBailian = useCallback(
    () => bailianInstallCmd.execute(),
    [bailianInstallCmd],
  );

  const loginBailian = useCallback(
    () => bailianLoginCmd.execute(),
    [bailianLoginCmd],
  );

  const logoutBailian = useCallback(
    () => bailianLogoutCmd.execute(),
    [bailianLogoutCmd],
  );

  const setBailianOpenApi = useCallback(
    (accessKeyId: string, accessKeySecret: string) =>
      bailianSetOpenApiCmd.execute({ accessKeyId, accessKeySecret }),
    [bailianSetOpenApiCmd],
  );

  const kimiStatus = useCallback(
    () => kimiStatusCmd.execute(),
    [kimiStatusCmd],
  );

  const loginKimi = useCallback(
    () => kimiLoginCmd.execute(),
    [kimiLoginCmd],
  );

  const logoutKimi = useCallback(
    () => kimiLogoutCmd.execute(),
    [kimiLogoutCmd],
  );

  const grokStatus = useCallback(
    () => grokStatusCmd.execute(),
    [grokStatusCmd],
  );

  const installGrok = useCallback(
    () => grokInstallCmd.execute(),
    [grokInstallCmd],
  );

  const loginGrok = useCallback(
    () => grokLoginCmd.execute(),
    [grokLoginCmd],
  );

  const logoutGrok = useCallback(
    () => grokLogoutCmd.execute(),
    [grokLogoutCmd],
  );

  const codexStatus = useCallback(
    () => codexStatusCmd.execute(),
    [codexStatusCmd],
  );

  const installCodex = useCallback(
    () => codexInstallCmd.execute(),
    [codexInstallCmd],
  );

  const loginCodex = useCallback(
    () => codexLoginStartCmd.execute(),
    [codexLoginStartCmd],
  );

  const cancelCodexLogin = useCallback(() => {
    void codexLoginCancelCmd.execute();
  }, [codexLoginCancelCmd]);

  const logoutCodex = useCallback(
    () => codexLogoutCmd.execute(),
    [codexLogoutCmd],
  );

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

  const setLaunchOnStartup = useCallback(
    async (enabled: boolean) => {
      const updated = await launchOnStartupCmd.execute({ enabled });
      if (updated) setSettings(updated);
    },
    [launchOnStartupCmd],
  );

  const setMinimalView = useCallback(
    async (enabled: boolean) => {
      // Flip locally first so the view resizes the instant the toggle is hit;
      // the backend write is just persistence and shouldn't gate the UI. The
      // round-trip result still overwrites it below to stay authoritative.
      setSettings((prev) => (prev ? { ...prev, minimalView: enabled } : prev));
      const updated = await minimalViewCmd.execute({ enabled });
      if (updated) setSettings(updated);
    },
    [minimalViewCmd],
  );

  const setTooltipProvider = useCallback(
    async (provider: TooltipProvider) => {
      const updated = await tooltipProviderCmd.execute({ provider });
      if (updated) setSettings(updated);
    },
    [tooltipProviderCmd],
  );

  const setWindowMode = useCallback(
    async (mode: WindowMode) => {
      const updated = await windowModeCmd.execute({ mode });
      if (updated) setSettings(updated);
    },
    [windowModeCmd],
  );

  const setHiddenProviders = useCallback(
    async (providers: string[]) => {
      const updated = await hiddenProvidersCmd.execute({ providers });
      if (updated) setSettings(updated);
    },
    [hiddenProvidersCmd],
  );

  const setAutoRotate = useCallback(
    async (enabled: boolean) => {
      const updated = await autoRotateCmd.execute({ enabled });
      if (updated) setSettings(updated);
    },
    [autoRotateCmd],
  );

  const setRotateSecs = useCallback(
    async (secs: number) => {
      const updated = await rotateSecsCmd.execute({ secs });
      if (updated) setSettings(updated);
    },
    [rotateSecsCmd],
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

  // Re-read settings after an out-of-band change (e.g. a completed Copilot
  // device-flow connect updates the stored token server-side).
  const reloadSettings = useCallback(async () => {
    const view = await settingsCmd.execute();
    if (view) setSettings(view);
  }, [settingsCmd]);

  // Copilot device flow: start returns the user code + verification URL; poll
  // returns "pending" | "slow_down" | "connected" | "denied" | "expired" each tick.
  const connectCopilotStart = useCallback(
    () => copilotStartCmd.execute(),
    [copilotStartCmd],
  );
  const copilotPoll = useCallback(() => copilotPollCmd.execute(), [copilotPollCmd]);
  // Abandon an in-progress connect server-side so the next attempt starts fresh.
  const copilotCancel = useCallback(() => {
    void copilotCancelCmd.execute();
  }, [copilotCancelCmd]);

  const disconnectCopilot = useCallback(async () => {
    const updated = await disconnectCopilotCmd.execute();
    if (updated) {
      setSettings(updated);
      await refresh();
    }
  }, [disconnectCopilotCmd, refresh]);

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
    setLaunchOnStartup,
    setMinimalView,
    setTooltipProvider,
    setWindowMode,
    setHiddenProviders,
    setAutoRotate,
    setRotateSecs,
    setGlmEndpoint,
    setApiKey,
    clearApiKey,
    reloadSettings,
    connectCopilotStart,
    copilotPoll,
    copilotCancel,
    disconnectCopilot,
    refresh,
    claudeLoginStart,
    claudeLoginFinish,
    claudeLoginCancel,
    claudeLoginBusy: claudeLoginStartCmd.isLoading || claudeLoginFinishCmd.isLoading,
    claudeLoginError: claudeLoginFinishCmd.error ?? claudeLoginStartCmd.error,
    claudeSignOut,
    claudeSignOutError: claudeSignOutCmd.error,
    bailianStatus,
    installBailian,
    bailianInstallBusy: bailianInstallCmd.isLoading,
    bailianInstallError: bailianInstallCmd.error,
    loginBailian,
    bailianLoginBusy: bailianLoginCmd.isLoading,
    bailianLoginError: bailianLoginCmd.error,
    logoutBailian,
    bailianLogoutBusy: bailianLogoutCmd.isLoading,
    bailianLogoutError: bailianLogoutCmd.error,
    setBailianOpenApi,
    bailianSetOpenApiBusy: bailianSetOpenApiCmd.isLoading,
    bailianSetOpenApiError: bailianSetOpenApiCmd.error,
    kimiStatus,
    loginKimi,
    kimiLoginBusy: kimiLoginCmd.isLoading,
    kimiLoginError: kimiLoginCmd.error,
    logoutKimi,
    kimiLogoutBusy: kimiLogoutCmd.isLoading,
    kimiLogoutError: kimiLogoutCmd.error,
    grokStatus,
    installGrok,
    grokInstallBusy: grokInstallCmd.isLoading,
    grokInstallError: grokInstallCmd.error,
    loginGrok,
    grokLoginBusy: grokLoginCmd.isLoading,
    grokLoginError: grokLoginCmd.error,
    logoutGrok,
    grokLogoutBusy: grokLogoutCmd.isLoading,
    grokLogoutError: grokLogoutCmd.error,
    codexStatus,
    installCodex,
    codexInstallBusy: codexInstallCmd.isLoading,
    codexInstallError: codexInstallCmd.error,
    loginCodex,
    cancelCodexLogin,
    codexLoginBusy: codexLoginStartCmd.isLoading,
    codexLoginError: codexLoginStartCmd.error,
    logoutCodex,
    codexLogoutBusy: codexLogoutCmd.isLoading,
    codexLogoutError: codexLogoutCmd.error,
    isLoading: usageCmd.isLoading,
    error: usageCmd.error,
    keyError: setKeyCmd.error,
  };
}
