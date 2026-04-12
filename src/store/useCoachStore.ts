import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type {
  CoachMode,
  CoachRule,
  CoachSnapshot,
  CoachView,
  EngineMode,
  HookStatus,
  ModelConfig,
  PathStatus,
  SessionSnapshot,
  Theme,
  TokenStatus,
} from "../types";

interface CoachState {
  sessions: SessionSnapshot[];
  priorities: string[];
  port: number;
  theme: Theme;
  model: ModelConfig;
  tokenStatus: Record<string, TokenStatus>;
  engineMode: EngineMode;
  rules: CoachRule[];
  hookStatus: HookStatus | null;
  codexHookStatus: HookStatus | null;
  cursorHookStatus: HookStatus | null;
  pathStatus: PathStatus | null;
  observerCapableProviders: string[];
  autoUninstallHooksOnExit: boolean;
  llmLogDir: string | null;
  modelError: string | null;
  modelValidating: boolean;
  initialized: boolean;
  initError: string | null;
  view: CoachView;
  selectedSessionId: string | null;
}

interface CoachActions {
  init: () => Promise<void>;
  setSessionMode: (sessionId: string, mode: CoachMode) => Promise<void>;
  setAllMode: (mode: CoachMode) => Promise<void>;
  setPriorities: (priorities: string[]) => Promise<void>;
  addPriority: (priority: string) => Promise<void>;
  removePriority: (index: number) => Promise<void>;
  movePriority: (index: number, direction: "up" | "down") => Promise<void>;
  setTheme: (theme: Theme) => Promise<void>;
  setApiToken: (provider: string, token: string) => Promise<void>;
  setModel: (model: ModelConfig) => Promise<void>;
  setView: (view: CoachView) => void;
  openSession: (sessionId: string) => void;
  setEngineMode: (mode: EngineMode) => Promise<void>;
  setRules: (rules: CoachRule[]) => Promise<void>;
  toggleRule: (id: string) => Promise<void>;
  refreshHookStatus: () => Promise<void>;
  installHooks: () => Promise<void>;
  uninstallHooks: () => Promise<void>;
  refreshCodexHookStatus: () => Promise<void>;
  installCodexHooks: () => Promise<void>;
  uninstallCodexHooks: () => Promise<void>;
  refreshCursorHookStatus: () => Promise<void>;
  installCursorHooks: () => Promise<void>;
  uninstallCursorHooks: () => Promise<void>;
  refreshPathStatus: () => Promise<void>;
  installPath: () => Promise<void>;
  uninstallPath: () => Promise<void>;
  setAutoUninstallHooksOnExit: (enabled: boolean) => Promise<void>;
  setInterventionMuted: (sessionId: string, muted: boolean) => Promise<void>;
}

type CoachStore = CoachState & CoachActions;

function applyThemeClass(theme: Theme) {
  const root = document.documentElement;
  if (theme === "dark") {
    root.classList.add("dark");
  } else if (theme === "light") {
    root.classList.remove("dark");
  } else {
    const prefersDark = window.matchMedia(
      "(prefers-color-scheme: dark)",
    ).matches;
    root.classList.toggle("dark", prefersDark);
  }
}

export type { TokenSource, TokenStatus, ModelConfig, SessionSnapshot, SessionClient, ActivityEntry, HookStatus, PathStatus, EngineMode, CoachRule, CoachView, CoachUsage } from "../types";

export const useCoachStore = create<CoachStore>((set, get) => ({
  sessions: [],
  priorities: [],
  port: 7700,
  theme: "system",
  model: { provider: "google", model: "gemini-2.5-flash" },
  tokenStatus: {},
  engineMode: "rules",
  rules: [],
  hookStatus: null,
  codexHookStatus: null,
  cursorHookStatus: null,
  pathStatus: null,
  observerCapableProviders: [],
  autoUninstallHooksOnExit: true,
  llmLogDir: null,
  modelError: null,
  modelValidating: false,
  initialized: false,
  initError: null,
  view: "main",
  selectedSessionId: null,

  init: async () => {
    if (get().initialized) return;

    try {
      const snapshot = await invoke<CoachSnapshot>("get_state");
      applyThemeClass(snapshot.theme);

      set({
        sessions: snapshot.sessions,
        priorities: snapshot.priorities,
        port: snapshot.port,
        theme: snapshot.theme,
        model: snapshot.model,
        tokenStatus: snapshot.token_status,
        engineMode: snapshot.coach_mode,
        rules: snapshot.rules,
        observerCapableProviders: snapshot.observer_capable_providers,
        autoUninstallHooksOnExit: snapshot.auto_uninstall_hooks_on_exit,
        llmLogDir: snapshot.llm_log_dir ?? null,
        initialized: true,
      });
    } catch (e) {
      set({ initError: String(e) });
      return;
    }

    get().refreshHookStatus();
    get().refreshCodexHookStatus();
    get().refreshCursorHookStatus();
    get().refreshPathStatus();

    await listen<CoachSnapshot>("coach-state-updated", (event) => {
      const s = event.payload;
      set({
        sessions: s.sessions,
        priorities: s.priorities,
        model: s.model,
        tokenStatus: s.token_status,
        engineMode: s.coach_mode,
        rules: s.rules,
        observerCapableProviders: s.observer_capable_providers,
        autoUninstallHooksOnExit: s.auto_uninstall_hooks_on_exit,
        llmLogDir: s.llm_log_dir ?? null,
      });
    });

    await listen<Theme>("coach-theme-changed", (event) => {
      applyThemeClass(event.payload);
      set({ theme: event.payload });
    });

    window
      .matchMedia("(prefers-color-scheme: dark)")
      .addEventListener("change", () => {
        if (get().theme === "system") applyThemeClass("system");
      });
  },

  setSessionMode: async (sessionId, mode) => {
    await invoke("set_session_mode", { sessionId, mode });
    set((s) => ({
      sessions: s.sessions.map((sess) =>
        sess.session_id === sessionId ? { ...sess, mode } : sess,
      ),
    }));
  },

  setAllMode: async (mode) => {
    await invoke("set_all_sessions_mode", { mode });
    set((s) => ({
      sessions: s.sessions.map((sess) => ({ ...sess, mode })),
    }));
  },

  setPriorities: async (priorities) => {
    await invoke("set_priorities", { priorities });
    set({ priorities });
  },

  addPriority: async (priority) => {
    const next = [...get().priorities, priority];
    await get().setPriorities(next);
  },

  removePriority: async (index) => {
    const next = get().priorities.filter((_, i) => i !== index);
    await get().setPriorities(next);
  },

  movePriority: async (index, direction) => {
    const arr = [...get().priorities];
    const target = direction === "up" ? index - 1 : index + 1;
    if (target < 0 || target >= arr.length) return;
    [arr[index], arr[target]] = [arr[target], arr[index]];
    await get().setPriorities(arr);
  },

  setTheme: async (theme) => {
    await invoke("set_theme", { theme });
    applyThemeClass(theme);
    set({ theme });
  },

  setApiToken: async (provider, token) => {
    await invoke("set_api_token", { provider, token });
  },

  setModel: async (model) => {
    await invoke("set_model", { model });
    set({ model, modelError: null, modelValidating: true });
    try {
      await invoke("validate_model", {
        provider: model.provider,
        model: model.model,
      });
      set({ modelError: null, modelValidating: false });
    } catch (e) {
      set({ modelError: String(e), modelValidating: false });
    }
  },

  setView: (view) => set({ view }),

  openSession: (sessionId) => set({ selectedSessionId: sessionId, view: "session" }),

  setEngineMode: async (coachMode) => {
    await invoke("set_coach_mode", { coachMode });
    set({ engineMode: coachMode });
  },

  setRules: async (rules) => {
    await invoke("set_rules", { rules });
    set({ rules });
  },

  toggleRule: async (id) => {
    const rules = get().rules.map((r) =>
      r.id === id ? { ...r, enabled: !r.enabled } : r,
    );
    await get().setRules(rules);
  },

  refreshHookStatus: async () => {
    const hookStatus = await invoke<HookStatus>("get_hook_status");
    set({ hookStatus });
  },

  installHooks: async () => {
    const hookStatus = await invoke<HookStatus>("install_hooks");
    set({ hookStatus });
  },

  uninstallHooks: async () => {
    const hookStatus = await invoke<HookStatus>("uninstall_hooks");
    set({ hookStatus });
  },

  refreshCodexHookStatus: async () => {
    const codexHookStatus = await invoke<HookStatus>("get_codex_hook_status");
    set({ codexHookStatus });
  },

  installCodexHooks: async () => {
    const codexHookStatus = await invoke<HookStatus>("install_codex_hooks");
    set({ codexHookStatus });
  },

  uninstallCodexHooks: async () => {
    const codexHookStatus = await invoke<HookStatus>("uninstall_codex_hooks");
    set({ codexHookStatus });
  },

  refreshCursorHookStatus: async () => {
    const cursorHookStatus = await invoke<HookStatus>("get_cursor_hook_status");
    set({ cursorHookStatus });
  },

  installCursorHooks: async () => {
    const cursorHookStatus = await invoke<HookStatus>("install_cursor_hooks");
    set({ cursorHookStatus });
  },

  uninstallCursorHooks: async () => {
    const cursorHookStatus = await invoke<HookStatus>("uninstall_cursor_hooks");
    set({ cursorHookStatus });
  },

  refreshPathStatus: async () => {
    const pathStatus = await invoke<PathStatus>("get_path_status");
    set({ pathStatus });
  },

  installPath: async () => {
    const pathStatus = await invoke<PathStatus>("install_path");
    set({ pathStatus });
  },

  uninstallPath: async () => {
    const pathStatus = await invoke<PathStatus>("uninstall_path");
    set({ pathStatus });
  },

  setAutoUninstallHooksOnExit: async (enabled) => {
    await invoke("set_auto_uninstall_hooks_on_exit", { enabled });
    set({ autoUninstallHooksOnExit: enabled });
  },

  setInterventionMuted: async (sessionId, muted) => {
    await invoke("set_intervention_muted", { sessionId, muted });
    set((s) => ({
      sessions: s.sessions.map((sess) =>
        sess.session_id === sessionId ? { ...sess, intervention_muted: muted } : sess,
      ),
    }));
  },
}));
