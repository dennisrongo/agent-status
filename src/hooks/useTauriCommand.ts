import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface State<T> {
  data: T | null;
  isLoading: boolean;
  error: string | null;
}

/**
 * Generic typed wrapper around Tauri `invoke`. Components compose per-domain
 * hooks on top of this rather than calling `invoke` inline.
 */
export function useTauriCommand<T>(command: string) {
  const [state, setState] = useState<State<T>>({
    data: null,
    isLoading: false,
    error: null,
  });

  const execute = useCallback(
    async (args?: Record<string, unknown>): Promise<T | null> => {
      setState((s) => ({ ...s, isLoading: true, error: null }));
      try {
        const data = await invoke<T>(command, args);
        setState({ data, isLoading: false, error: null });
        return data;
      } catch (e) {
        const error = e instanceof Error ? e.message : String(e);
        setState((s) => ({ ...s, isLoading: false, error }));
        return null;
      }
    },
    [command],
  );

  return { ...state, execute };
}
