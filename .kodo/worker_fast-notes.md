# Coach — worker notes

## Verify

- `cd src-tauri && cargo test` — default build has no `pycoach` feature.
- Optional sidecar smoke: `cargo test --features pycoach --test pycoach_sidecar` (needs `uv` on PATH and sibling checkout `ilya/pycoach`).

## Hooks / settings

- `hooks_user_enabled` and `cursor_hooks_user_enabled` record install intent; they survive `auto_uninstall_hooks_on_exit` (default true). Startup `sync_managed_*` reinstalls from intent and migrates legacy “hooks on disk, flag false” once.

## Prompts

- Templates live in `src-tauri/prompts/*.txt`, embedded via `prompts.rs`. Override at dev time: `COACH_PROMPTS_DIR` → read fresh each call; missing file errors (no silent fallback).

## Refactor backlog (backend review)

- Dedupe provider dispatch across chat / extract / chain / `session_send` in `llm.rs`.
- Reduce repeated PID wrappers and mutate-save-emit in `server.rs`.
- Centralize session construction / snapshot mapping in `state.rs`.
- Simplify `run_stop` telemetry and lock phases.
- Unify config mutation paths (`commands.rs` vs `server.rs`).
- Share GUI vs `serve()` bootstrap in `lib.rs`.
