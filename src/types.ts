// Shared types between the Rust backend and the TypeScript frontend.
//
// These types mirror the Rust structs serialised over the Tauri IPC
// boundary. When changing a type here, update the Rust source too:
//
//   SessionSnapshot, CoachSnapshot  ->  coach-core/src/state/snapshot.rs
//   ModelConfig, EngineMode, CoachRule  ->  coach-core/src/settings/mod.rs
//   HookStatus, HookEntryStatus  ->  coach-core/src/settings/hooks.rs
//   PathStatus  ->  coach-core/src/path_install.rs
//   CoachMode, Theme, ActivityEntry, SessionClient, CoachUsage
//                                  ->  coach-core/src/state/mod.rs

// ── Enums / aliases ────────────────────────────────────────────────────

export type CoachMode = "present" | "away";
export type Theme = "light" | "dark" | "system";
export type TokenSource = "user" | "env" | "none";
export type EngineMode = "rules" | "llm";
export type SessionClient = "claude" | "cursor" | "codex";
export type CoachView = "main" | "settings" | "hooks" | "session" | "dev";

// ── Data types ─────────────────────────────────────────────────────────

export interface CoachRule {
  id: string;
  enabled: boolean;
}

export interface ActivityEntry {
  timestamp: string;
  hook_event: string;
  action: string;
  detail: string | null;
}

export interface CoachUsage {
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
}

export interface ModelConfig {
  provider: string;
  model: string;
}

export interface TokenStatus {
  source: TokenSource;
  env_var?: string;
}

export interface HookEntryStatus {
  event: string;
  url: string;
  installed: boolean;
}

export interface HookStatus {
  installed: boolean;
  path: string;
  hooks: HookEntryStatus[];
}

export interface PathStatus {
  install_path: string;
  installed: boolean;
  target: string | null;
  matches_current_exe: boolean;
  on_path: boolean;
}

// ── Snapshot types (Rust -> TS via Tauri events) ───────────────────────

export interface SessionSnapshot {
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
  /** "empty" | "server_id" | "history" — which backend the chain is using. */
  coach_chain_kind: string;
  /** Number of messages currently held in the coach's conversation. */
  coach_chain_messages: number;
  /** Successful coach LLM calls (observer + chained stop). */
  coach_calls: number;
  /** Failed coach LLM calls. */
  coach_errors: number;
  coach_last_called_at: string | null;
  coach_last_latency_ms: number | null;
  coach_last_usage: CoachUsage | null;
  coach_total_usage: CoachUsage;
  activity: ActivityEntry[];
  /** Number of Agent tool calls currently in-flight. */
  active_agents: number;
  client: SessionClient;
  is_worktree: boolean;
  intervention_muted: boolean;
  pending_intervention: string | null;
  intervention_count: number;
  observer_dropped: number;
  coach_last_system_prompt: string | null;
  coach_last_user_message: string | null;
}

export interface CoachSnapshot {
  sessions: SessionSnapshot[];
  priorities: string[];
  port: number;
  theme: Theme;
  model: ModelConfig;
  token_status: Record<string, TokenStatus>;
  coach_mode: EngineMode;
  rules: CoachRule[];
  observer_capable_providers: string[];
  auto_uninstall_hooks_on_exit: boolean;
}
