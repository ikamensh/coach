import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type CoachMode = "present" | "away";
type Theme = "light" | "dark" | "system";
type TokenSource = "user" | "env" | "none";
type EngineMode = "rules" | "llm";
type CoachView = "main" | "settings" | "hooks" | "session" | "dev";

interface CoachRule {
  id: string;
  enabled: boolean;
}

interface ActivityEntry {
  timestamp: string;
  hook_event: string;
  action: string;
  detail: string | null;
}

type SessionClient = "claude" | "cursor";

interface CoachUsage {
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
}

interface SessionSnapshot {
  /** OS PID — stable across /clear, the canonical identity for a window. */
  pid: number;
  /** Current conversation id — changes on /clear. */
  session_id: string;
  mode: CoachMode;
  /** Launch directory of the window. Set once on first observation and frozen. */
  cwd: string | null;
  last_event: string;
  event_count: number;
  display_name: string;
  started_at: string;
  duration_secs: number;
  tool_counts: Record<string, number>;
  stop_count: number;
  stop_blocked_count: number;
  coach_last_assessment: string | null;
  coach_last_error: string | null;
  /** Periodic LLM-generated 4-words-or-fewer topic. Frontend prefers this over `display_name`. */
  coach_session_title: string | null;
  /// "empty" | "openai" | "anthropic" — which backend the chain is using.
  coach_chain_kind: string;
  /// Number of messages currently held in the coach's conversation.
  coach_chain_messages: number;
  /// Successful coach LLM calls (observer + chained stop).
  coach_calls: number;
  /// Failed coach LLM calls.
  coach_errors: number;
  coach_last_called_at: string | null;
  coach_last_latency_ms: number | null;
  coach_last_usage: CoachUsage | null;
  coach_total_usage: CoachUsage;
  activity: ActivityEntry[];
  client: SessionClient;
}

interface ModelConfig {
  provider: string;
  model: string;
}

interface TokenStatus {
  source: TokenSource;
  env_var?: string;
}

interface CoachSnapshot {
  sessions: SessionSnapshot[];
  priorities: string[];
  port: number;
  theme: Theme;
  model: ModelConfig;
  token_status: Record<string, TokenStatus>;
  coach_mode: EngineMode;
  rules: CoachRule[];
  auto_uninstall_hooks_on_exit: boolean;
}

interface HookEntryStatus {
  event: string;
  url: string;
  installed: boolean;
}

interface HookStatus {
  installed: boolean;
  path: string;
  hooks: HookEntryStatus[];
}

interface PathStatus {
  install_path: string;
  installed: boolean;
  target: string | null;
  matches_current_exe: boolean;
  on_path: boolean;
}

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
  cursorHookStatus: HookStatus | null;
  pathStatus: PathStatus | null;
  autoUninstallHooksOnExit: boolean;
  modelError: string | null;
  modelValidating: boolean;
  initialized: boolean;
  initError: string | null;
  view: CoachView;
  selectedPid: number | null;
}

interface CoachActions {
  init: () => Promise<void>;
  setSessionMode: (pid: number, mode: CoachMode) => Promise<void>;
  setAllMode: (mode: CoachMode) => Promise<void>;
  setPriorities: (priorities: string[]) => Promise<void>;
  addPriority: (priority: string) => Promise<void>;
  removePriority: (index: number) => Promise<void>;
  movePriority: (index: number, direction: "up" | "down") => Promise<void>;
  setTheme: (theme: Theme) => Promise<void>;
  setApiToken: (provider: string, token: string) => Promise<void>;
  setModel: (model: ModelConfig) => Promise<void>;
  setView: (view: CoachView) => void;
  openSession: (pid: number) => void;
  setEngineMode: (mode: EngineMode) => Promise<void>;
  setRules: (rules: CoachRule[]) => Promise<void>;
  toggleRule: (id: string) => Promise<void>;
  refreshHookStatus: () => Promise<void>;
  installHooks: () => Promise<void>;
  uninstallHooks: () => Promise<void>;
  refreshCursorHookStatus: () => Promise<void>;
  installCursorHooks: () => Promise<void>;
  uninstallCursorHooks: () => Promise<void>;
  refreshPathStatus: () => Promise<void>;
  installPath: () => Promise<void>;
  uninstallPath: () => Promise<void>;
  setAutoUninstallHooksOnExit: (enabled: boolean) => Promise<void>;
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

export type { TokenSource, TokenStatus, ModelConfig, SessionSnapshot, SessionClient, ActivityEntry, HookStatus, PathStatus, EngineMode, CoachRule, CoachView, CoachUsage };

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
  cursorHookStatus: null,
  pathStatus: null,
  autoUninstallHooksOnExit: true,
  modelError: null,
  modelValidating: false,
  initialized: false,
  initError: null,
  view: "main",
  selectedPid: null,

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
        autoUninstallHooksOnExit: snapshot.auto_uninstall_hooks_on_exit,
        initialized: true,
      });
    } catch (e) {
      set({ initError: String(e) });
      return;
    }

    get().refreshHookStatus();
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
        autoUninstallHooksOnExit: s.auto_uninstall_hooks_on_exit,
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

  setSessionMode: async (pid, mode) => {
    await invoke("set_session_mode", { pid, mode });
    set((s) => ({
      sessions: s.sessions.map((sess) =>
        sess.pid === pid ? { ...sess, mode } : sess,
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

  openSession: (pid) => set({ selectedPid: pid, view: "session" }),

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
}));
