import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type CoachMode = "present" | "away";
type Theme = "light" | "dark" | "system";
type TokenSource = "user" | "env" | "none";

interface ActivityEntry {
  timestamp: string;
  session_id: string;
  hook_event: string;
  action: string;
  detail: string | null;
}

interface SessionSnapshot {
  session_id: string;
  mode: CoachMode;
  cwd: string | null;
  last_event: string;
  event_count: number;
  display_name: string;
  started_at: string;
  duration_secs: number;
  tool_counts: Record<string, number>;
  stop_count: number;
  stop_blocked_count: number;
  cwd_history: string[];
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
  activity_log: ActivityEntry[];
  port: number;
  theme: Theme;
  model: ModelConfig;
  token_status: Record<string, TokenStatus>;
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

interface CoachState {
  sessions: SessionSnapshot[];
  priorities: string[];
  activityLog: ActivityEntry[];
  port: number;
  theme: Theme;
  model: ModelConfig;
  tokenStatus: Record<string, TokenStatus>;
  hookStatus: HookStatus | null;
  modelError: string | null;
  modelValidating: boolean;
  initialized: boolean;
  view: "main" | "settings" | "hooks" | "session";
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
  setView: (view: "main" | "settings" | "hooks" | "session") => void;
  selectSession: (id: string | null) => void;
  refreshHookStatus: () => Promise<void>;
  installHooks: () => Promise<void>;
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

export type { TokenSource, TokenStatus, ModelConfig, SessionSnapshot, HookStatus };

export const useCoachStore = create<CoachStore>((set, get) => ({
  sessions: [],
  priorities: [],
  activityLog: [],
  port: 7700,
  theme: "system",
  model: { provider: "google", model: "gemini-2.5-flash" },
  tokenStatus: {},
  hookStatus: null,
  modelError: null,
  modelValidating: false,
  initialized: false,
  view: "main",
  selectedSessionId: null,

  init: async () => {
    if (get().initialized) return;

    const snapshot = await invoke<CoachSnapshot>("get_state");
    applyThemeClass(snapshot.theme);

    set({
      sessions: snapshot.sessions,
      priorities: snapshot.priorities,
      activityLog: snapshot.activity_log,
      port: snapshot.port,
      theme: snapshot.theme,
      model: snapshot.model,
      tokenStatus: snapshot.token_status,
      initialized: true,
    });

    get().refreshHookStatus();

    await listen<CoachSnapshot>("coach-state-updated", (event) => {
      const s = event.payload;
      set({
        sessions: s.sessions,
        priorities: s.priorities,
        activityLog: s.activity_log,
        model: s.model,
        tokenStatus: s.token_status,
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

  selectSession: (id) => set({ selectedSessionId: id, view: id ? "session" : "main" }),

  refreshHookStatus: async () => {
    const hookStatus = await invoke<HookStatus>("get_hook_status");
    set({ hookStatus });
  },

  installHooks: async () => {
    const hookStatus = await invoke<HookStatus>("install_hooks");
    set({ hookStatus });
  },
}));
