# Coach — worker notes

## Build

- Workspace root: `coach/Cargo.toml` — artifacts land in **`coach/target/`**, not `src-tauri/target/`.
- Release CLI binary: `cargo build --release` from `coach/` (or `src-tauri/`) → `target/release/coach`.

## Verify

- Strict clippy: `cd src-tauri && cargo clippy --all-targets --all-features -- -D warnings`.
- Rust tests: `cargo test` (repo root) — 209 passed, 21 ignored in a baseline run (2026-04-10); `cargo test --all-features` adds `pycoach_sidecar` when `uv` + sibling `ilya/pycoach` available.
- Optional sidecar only: `cargo test --features pycoach --test pycoach_sidecar`.
- Frontend: repo root `npm test`, `npm run build`.
- **Discovery (Stage 1):** `.kodo/test-report.md` (setup + CLI smoke), `.kodo/test-coverage.md` (coverage matrix; Codex HTTP routes pending in tests).
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

## CLI config & path (E2E, release binary)

- **No `config list`:** `coach config list` → `usage: coach config <get|set>`. “List” settings with **`coach config get`** (full JSON) or **`coach config get all`**, or keyed **`priorities` / `model` / `coach-mode` / `port` / `rules`**.
- **Reads vs writes:** **`config get` always loads `~/.coach/settings.json`** (`Settings::load`). **`config set`** uses **HTTP** to `/api/config/...` when `http://127.0.0.1:{port}/version` succeeds, else **writes the file** — tested with **daemon down** (file path only). If Coach is running, expect **`config get` to reflect disk**, not necessarily the same probe as `coach status` (HTTP snapshot).
- **`coach-mode` output:** `config get coach-mode` prints **Rust `Debug`** (`Llm`, `Rules`); **`config set`** expects lowercase **`rules` \| `llm`**.
- **`path status`:** **`matches_running`** is false if `~/.local/bin/coach` points at a **different** binary than the one you invoked (e.g. shim → `/Applications/Coach.app/...` vs workspace `target/release/coach`). **`path install --dir <dir>`** puts a symlink at `<dir>/coach` → `current_exe()`; **`path status`** still reports the **default** install dir only (`~/.local/bin` on macOS).
- **E2E nits:** `coach config get <key> …` **ignores** trailing extra args (no error). **`coach path`** with no subcommand behaves like **`path status`** (undocumented). **`serve --port N`** **persists** `port` to `~/.coach/settings.json` before bind — can surprise one-off runs.

## Security / CLI (quick reference)

- **Help text:** Top-level `coach`, `coach help`, `coach -h`, `coach --help` print full usage. **`coach serve --help` / `-h` / trailing `help`** print `serve`-specific help and exit 0 without binding (regression tests in `cli.rs`).
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
