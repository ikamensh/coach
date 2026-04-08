# Coach — worker notes

## Verify

- Strict clippy: `cd src-tauri && cargo clippy --all-targets --all-features -- -D warnings`.
- Rust tests: `cargo test` ~184 passed, 17 ignored (default); `cargo test --all-features` ~186 passed, 17 ignored (adds `pycoach_sidecar` when `uv` + sibling `ilya/pycoach` available).
- Optional sidecar only: `cargo test --features pycoach --test pycoach_sidecar`.
- Frontend: repo root `npm test`, `npm run build`.
- **Kodo improve (Stage 3):** auto-fixes land as `chore: auto-fix issues found by kodo improve`; triage report is `improve-report.md` under `~/.kodo/runs/<run_id>/` (ensure Rust/npm counts in the report match actual `cargo test` / `npm test` output).

## Frontend / integration (Stage 2 review)

- **Contract drift:** `CoachSnapshot` includes `observer_capable_providers` in Rust; TS types omit it — sync risk for consumers.
- **Agents:** Claude + Cursor monitoring paths overlap; Cursor hook payload handling is defensive / shape-probing (brittle if hooks change).
- **State:** Optimistic Zustand patches can lose to backend snapshot broadcasts → possible flicker or stale UI.
- **Perf / history:** Whole-snapshot refresh churn, per-row timers, unbounded observer chain history — watch memory and render cost.

## Hooks / settings

- `hooks_user_enabled` and `cursor_hooks_user_enabled` record install intent; they survive `auto_uninstall_hooks_on_exit` (default true). Startup `sync_managed_*` reinstalls from intent and migrates legacy “hooks on disk, flag false” once.

## Prompts

- Templates live in `src-tauri/prompts/*.txt`, embedded via `prompts.rs`. Override at dev time: `COACH_PROMPTS_DIR` → read fresh each call; missing file errors (no silent fallback).

## Security / CLI (quick reference)

- Hook + REST surface binds **127.0.0.1** only (`server.rs` `start_server` / `serve_on_listener`, `lib.rs` `serve`). No auth on `/api/*` or hooks — any local process can call them.
- **`coach config get`** (no key / `all`) prints full `Settings` JSON including **`api_tokens`** (`cli.rs` → `settings.rs` shape). Live HTTP snapshot omits raw tokens (`state.rs` `CoachSnapshot` uses `token_status` only).
- Tokens on CLI: `config set api-token …` passes the secret in **argv** (visible in `ps`). Plaintext **`~/.coach/settings.json`** (`settings.rs` `save_to`).
- GUI stderr is redirected to **`~/Library/Logs/Coach/`** (etc.) (`logging.rs`) — verify new `eprintln!` sites don’t leak prompts or provider error bodies.

## Refactor backlog (backend review)

- Dedupe provider dispatch across chat / extract / chain / `session_send` in `llm.rs`.
- Reduce repeated PID wrappers and mutate-save-emit in `server.rs`.
- Centralize session construction / snapshot mapping in `state.rs`.
- Simplify `run_stop` telemetry and lock phases.
- Unify config mutation paths (`commands.rs` vs `server.rs`).
- Share GUI vs `serve()` bootstrap in `lib.rs`.
