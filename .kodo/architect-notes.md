# Architect Notes — Coach

## Project Shape
- Tauri 2 desktop app: Rust backend (Axum HTTP server) + React/TS frontend
- Dual execution: GUI via `run()` (Tauri builder) and headless via `serve()` (CLI daemon)
- Hook-driven: Claude Code and Cursor send HTTP hooks; Coach observes/intervenes
- Multi-provider LLM: OpenAI (native chain), Anthropic (client-side + cache), Google (client-side, no cache)

## Key Files
- `lib.rs` (257 lines) — two entry points: `run()` and `serve()`
- `server.rs` (~894 lines) — Axum routes, hook handlers, fire-and-forget LLM calls
- `server/cursor.rs` — Cursor-specific payload extraction + mark_cursor re-emit
- `llm.rs` (~1099 lines) — provider dispatch, chain implementations, observer/stop/namer
- `state.rs` (~800 lines) — CoachState, SessionState, snapshot serialization
- `settings.rs` (~800 lines) — persistence, hook management (Claude + Cursor)
- `commands.rs` (331 lines) — Tauri command handlers (mirror HTTP API)
- Frontend types: `src/store/useCoachStore.ts` (single file, all TS interfaces)

## Architectural Patterns
- State: `Arc<RwLock<CoachState>>` shared across HTTP server, scanner, Tauri
- Two API surfaces for same mutations: Tauri commands + Axum REST endpoints
- PID resolution: TCP peer port → PID via netstat2 (prod) or hash (test)
- Cursor sessions: synthetic PIDs via `fake_pid_for_sid()`, shim script workaround
- LLM dispatch: `session_send()` is the unified primitive; `chat()` / `extract_one()` are one-shot
- Provider client created fresh per-call (no pooling)

## Known Risks (2026-04-08 deep review)
1. **Observer chain race**: prev_chain cloned under lock, observer spawned async, writes back later. Two rapid PostToolUse events can fork the chain.
2. **TS↔Rust contract drift**: `observer_capable_providers` present in Rust `CoachSnapshot` but missing from TS interface. No codegen — all manual sync.
3. **API tokens plaintext on disk** — `~/.coach/settings.json` has `api_tokens` map in cleartext JSON.
4. **HTTP API has zero auth** — any local process can POST to /api/* and /hook/* endpoints.
5. **Settings file TOCTOU** — no file locking between read-modify-write cycles.
6. **Unbounded chain history** — Anthropic/Google histories grow without limit.
7. **Stop hook TOCTOU** — acquires write lock (phase 1), drops it, does LLM call, re-acquires (phase 2). Session could be removed or mode changed between phases.
8. **Cursor mark_cursor double-emit** — every first Cursor event emits snapshot twice (once from run_*, once from mark_cursor). Cosmetic but wasteful.

## Code Duplication (structural, not urgent)
- `chat()` and `extract_one()` have identical 4-arm match
- `chain_anthropic()` and `chain_gemini()` are ~90% identical
- `run()` and `serve()` duplicate init sequence
- Tauri commands and Axum API handlers duplicate mutation logic
- `run_stop()` is ~150 lines with 3 phases, nested error paths, duplicated telemetry writes
- `SessionState` has 20+ fields reset in apply_hook_event /clear path — no Default impl
- Claude and Cursor handlers converge on shared `run_*` functions (good), but payload extraction in cursor.rs probes many field names defensively
