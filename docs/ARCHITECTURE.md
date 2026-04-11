# Coach Architecture

Current shape after the refactor series `c7dedc5..ff4eb3c`. For the plan and history of how we got here, see `refactor.md`.

## Three layers

1. **`coach-core/`** — library crate that owns all domain logic: HTTP hook server, session state, LLM orchestration, services. GUI-agnostic and runnable headless via `coach serve`.
2. **`src-tauri/`** — Tauri app crate. Thin adapters around `coach-core`: `#[tauri::command]` wrappers that call `services::*`, a `TauriEmitter` that forwards state snapshots to the webview, tray + window lifecycle.
3. **`src/`** — React + TypeScript frontend. Zustand store listens for the `coach-state-updated` event and calls Tauri commands to mutate config.

The boundary between `coach-core` and the GUI is the `EventEmitter` trait in `coach-core/src/lib.rs`. Headless mode uses `NoopEmitter`; the GUI uses `TauriEmitter`.

## Key invariants

- **`state::mutate` is the only write path to `CoachState`.** It takes the write lock, runs the closure, snapshots, releases the lock, and emits the snapshot. Because there is one write primitive, the frontend can never miss an update.
- **Sessions are keyed by `session_id`**, the stable identifier that Claude/Codex/Cursor include in every hook payload. PID is metadata on `SessionState`, used only by the scanner for liveness and by the UI for display.
- **Every control-plane operation exists as exactly one function in `services.rs`.** Tauri commands and HTTP `/api/config/*` handlers both call into it. No operation has two implementations.
- **The observer consumer task's lifetime equals its session's lifetime.** It's spawned with the session's first tool observation and aborted by `impl Drop for SessionState` when the session is evicted.

## Diagram

```
External CLIs                                       Frontend / HTTP config       Feedback writers
─────────────                                       ────────────────────         ────────────────

┌──────────────────────────────┐                   ┌────────────────────┐       ┌──────────────────┐
│ Claude Code · Codex · Cursor │                   │ Tauri commands     │       │ Observer         │
└──────────────┬───────────────┘                   │ (src-tauri/        │       │ consumer task    │
               │ POST /hook/*                      │  commands.rs)      │       │ (server/         │
               │      /codex/hook/*                │                    │       │  observer.rs)    │
               │      /cursor/hook/*               │ HTTP /api/config/* │       │                  │
               ▼                                   │ (server/api.rs)    │       │ Scanner          │
┌──────────────────────────────┐                   └─────────┬──────────┘       │ (scanner.rs)     │
│ server/claude.rs             │                             │                   └────────┬─────────┘
│ server/codex.rs              │                             ▼                            │
│ server/cursor.rs             │                 ┌──────────────────────────┐             │
│                              │                 │ services.rs  (12 fns)    │             │
│ transport adapters:          │                 │                          │             │
│   parse raw payload          │                 │  set_priorities          │             │
│   → SessionEvent +           │                 │  set_model               │             │
│     SessionSource            │                 │  set_api_token           │             │
└──────────────┬───────────────┘                 │  set_theme               │             │
               │                                 │  set_coach_mode          │             │
               ▼                                 │  set_rules               │             │
┌──────────────────────────────┐                 │  set_hook_enabled        │             │
│ server/events.rs  dispatch() │                 │  set_auto_uninstall      │             │
│                              │                 │  set_session_mode        │             │
│ SessionEvent::               │                 │  set_intervention_muted  │             │
│   SessionStarted             │                 │  set_all_modes           │             │
│   UserPromptSubmitted        │                 │  toggle_default_mode     │             │
│   PermissionRequested        │                 └─────────────┬────────────┘             │
│   ToolStarting               │                               │                           │
│   ToolCompleted              │                               │                           │
│   StopRequested              │                               │                           │
│                              │                               │                           │
│ + SessionSource              │                               │                           │
│   { ClaudeCode, Codex,       │                               │                           │
│     Cursor }                 │                               │                           │
│                              │                               │                           │
│ → on_*_handler (one per      │                               │                           │
│   event, shared across all   │                               │                           │
│   three sources)             │                               │                           │
└──────────────┬───────────────┘                               │                           │
               │                                               │                           │
               └───────────────────────────────┬───────────────┴───────────────────────────┘
                                               │
                                               ▼
                  ┌─────────────────────────────────────────────────────┐
                  │ state::mutate(state, emitter, |s: &mut CoachState|) │
                  │                                                     │
                  │   acquire write lock                                │
                  │   run closure                                       │
                  │   snapshot                                          │
                  │   release lock                                      │
                  │   emitter.emit_state_update(&snapshot)              │
                  │                                                     │
                  │   THE ONLY write path for shared state.             │
                  └───────────────────────┬─────────────────────────────┘
                                          │
                                          ▼
         ┌──────────────────────────────────────────────────────────────┐
         │ CoachState  (behind Arc<RwLock<_>>)                           │
         │                                                                │
         │ ├─ sessions:  SessionRegistry                                  │
         │ │    inner: HashMap<SessionId, SessionState>                   │
         │ │    default_mode: CoachMode                                   │
         │ │    apply_hook_event · register_discovered_pid                │
         │ │    session_for_pid · session_key_for_pid                     │
         │ │    remove_dead_pids · mark_client · set_session_mode         │
         │ │    set_intervention_muted · set_all_modes · log              │
         │ │                                                               │
         │ ├─ config:    AppConfig  (= Settings, aliased)                 │
         │ │    priorities · model · api_tokens · rules · theme           │
         │ │    coach_mode · hooks_user_enabled · codex_hooks_…           │
         │ │    cursor_hooks_… · auto_uninstall · port                    │
         │ │    update_* methods + save() → ~/.coach/settings.json        │
         │ │                                                               │
         │ └─ services:  RuntimeServices                                  │
         │      http_client · env_tokens · mock_session_send              │
         │      llm_logger: Option<Arc<LlmLogger>>                        │
         │      pycoach:    Option<Arc<Pycoach>>  (feature-gated)         │
         └──────────────────────────────────────────────────────────────┘


Per-session state (one entry per SessionId in SessionRegistry.inner):

         ┌───────────────────────────────────────────────────────────────┐
         │ SessionState  (owned by value in the HashMap)                  │
         │                                                                 │
         │   session_id: SessionId       pid: u32   (metadata)             │
         │   last_event: Instant         tool_counts · activity            │
         │                                                                 │
         │   coach: SessionCoachState                                      │
         │     memory: CoachMemory                                         │
         │       chain · last_assessment · pending_intervention            │
         │       last_system_prompt · last_user_message · …                │
         │                                                                 │
         │     observer_tx:      mpsc::Sender<ObserverQueueItem>           │
         │                         (bounded, capacity 64)                  │
         │     observer_task:    Option<JoinHandle<()>>                    │
         │     observer_dropped: u64                                       │
         │                                                                 │
         │   impl Drop for SessionState:                                   │
         │     observer_task.abort()                                       │
         │     → consumer task dies when session is evicted                │
         └────────────────────────┬───────────────────────────────────────┘
                                  │ producer (on_tool_completed) calls
                                  │ try_send; on Full, drops + increments
                                  │ observer_dropped
                                  ▼
         ┌──────────────────────────────────────────────────────┐
         │ Observer consumer task   server/observer.rs           │
         │                                                        │
         │   recv ObserverQueueItem                               │
         │   → LlmCoach::observe_tool_use                         │
         │   → state::mutate writes back:                         │
         │       assessment, pending_intervention                 │
         └──────────────────────────┬───────────────────────────┘
                                    │
                                    ▼
         ┌────────────────────────┐  ┌─────────────────────────┐  ┌──────────────────┐
         │ LlmCoach   coach.rs    │  │ prompts.rs              │  │ Pycoach           │
         │                        │  │                         │  │ (feature-gated)   │
         │   observe_tool_use     │  │ #[cfg(debug_assertions)]│  │                   │
         │   evaluate_stop        │  │   read from disk        │  │ child process     │
         │   evaluate_stop_chain  │  │ else include_str!       │  │ HTTP handshake    │
         │   name_session         │  │                         │  │ via stdout;       │
         └───────────┬────────────┘  └─────────────────────────┘  │ stdin keeps liveness
                     │                                             └──────────────────┘
                     ▼
         ┌──────────────────────────────────────┐
         │ llm.rs  provider clients via `rig`   │
         │   Anthropic · OpenAI · Gemini · …    │
         └──────────────────────────────────────┘


Emit pipeline (state::mutate → GUI):

         state::mutate
              │
              ▼
         ┌──────────────────────────────────────────────────────┐
         │ EventEmitter trait   coach-core/src/lib.rs            │
         │   emit_state_update(&snapshot)                        │
         │   emit_theme_changed(&theme)                          │
         │                                                        │
         │   NoopEmitter      → headless (coach serve) and tests │
         │   TauriEmitter     → GUI                              │
         └──────────────────────────┬───────────────────────────┘
                                    │ Tauri app.emit(...)
                                    ▼
         ┌──────────────────────────────────────────────────────┐
         │ src/  React + TypeScript                              │
         │                                                        │
         │   useCoachStore.ts  (Zustand)                         │
         │     listen("coach-state-updated") → setState(snap)    │
         │     listen("coach-theme-changed")  → setTheme(...)    │
         │     selectedSessionId (stable), pid for display only  │
         │                                                        │
         │   App · SessionList · SessionDetail                   │
         │     invoke("set_…", { sessionId | payload })          │
         │       → commands.rs → services::*                     │
         └──────────────────────────────────────────────────────┘


Persistence layers:
   ~/.coach/settings.json      Settings::save on every config mutation
   $COACH_LLM_LOG_DIR/*.jsonl  per-call JSONL log (optional, via LlmLogger)
```

## File map

| Module | What lives here |
|---|---|
| `coach-core/src/state/mod.rs` | `CoachState`, `SessionRegistry`, `RuntimeServices`, `SessionState`, `SessionCoachState`, `CoachMemory`, `state::mutate()` |
| `coach-core/src/settings/mod.rs` | `Settings` (= `AppConfig` alias), `update_*` methods, disk persistence |
| `coach-core/src/services.rs` | 12 control-plane mutation functions |
| `coach-core/src/server.rs` | Axum router wiring, `AppState`, server entry point |
| `coach-core/src/server/claude.rs` · `codex.rs` · `cursor.rs` | Transport adapters (raw payload → `SessionEvent`) |
| `coach-core/src/server/events.rs` | `SessionEvent`, `SessionSource`, `dispatch()`, the `on_*` handlers |
| `coach-core/src/server/api.rs` | HTTP adapters for `/api/config/*` and `/api/sessions/{id}/mode` |
| `coach-core/src/server/observer.rs` | Per-session consumer task + `run_session_namer` |
| `coach-core/src/coach.rs` | `LlmCoach` entity: `observe_tool_use`, `evaluate_stop[_chained]`, `name_session` |
| `coach-core/src/llm.rs` | Provider clients (Anthropic, OpenAI, Gemini) via the `rig` crate |
| `coach-core/src/prompts.rs` | `#[cfg(debug_assertions)]` disk loader, release uses `include_str!` |
| `coach-core/src/scanner.rs` | Walks live PIDs, marks dead sessions, emits on change |
| `coach-core/src/replay.rs` | Replays logged JSONL back through `LlmCoach` |
| `coach-core/src/pycoach.rs` | Feature-gated Python sidecar launcher |
| `coach-core/src/lib.rs` | `EventEmitter` trait, `NoopEmitter`, `serve()` headless entry |
| `src-tauri/src/commands.rs` | Thin `#[tauri::command]` adapters that call `services::*` |
| `src-tauri/src/lib.rs` | `TauriEmitter`, `run()` entry, window lifecycle |
| `src-tauri/src/tray.rs` | Tray icon; calls `services::toggle_default_mode` |
| `src/store/useCoachStore.ts` | Zustand store, Tauri event listener |
| `src/types.ts` | `SessionSnapshot`, `CoachSnapshot`, etc. |
| `src/components/SessionList.tsx` · `SessionDetail.tsx` | React components, indexed by `session_id` |
