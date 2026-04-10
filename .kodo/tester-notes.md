# Coach — tester notes

## Environment (verified 2026-04-10)

- **Binary:** Workspace outputs `coach` at **`/coach/target/release/coach`** (or `target/debug/coach` after `cargo test`). Not under `src-tauri/target/` alone — use workspace root `target/`.
- **Rust:** `cargo test --workspace` — **211** passed, **21** ignored (e.g. coach-core 164+15, hook_integration 29+2, cli 17, scenario_replay 1+4). Use `--workspace`; bare `cargo test` from repo root can be misleading if you only read the last `test result` line.
- **Node:** `npm test` — Vitest **380** tests, 36 files. `npm run build` — green (~1.5s Vite).
- **CLI:** `./target/release/coach --version` matches workspace `0.1.70`.

## UX gotchas (reconfirmed)

- **No global subcommand-specific help** for most verbs (except `serve` now handles `--help` / `-h` / `help` — prints usage and exits 0 without binding).
- **Top-level** `coach`, `help`, `-h`, `--help` print full CLI usage.

## HTTP hook server E2E (binary, 2026-04-10)

- **Binary:** `coach/target/release/coach`; **`coach serve --port <PORT>`** persists `port` in `~/.coach/settings.json` so **`coach status`** targets the same daemon.
- **Claude routes** (`/hook/...`): use **TCP peer PID resolution** (curl works; stderr logs `resolved sid … → pid …`). Missing `session_id` in JSON becomes **`"unknown"`** (still HTTP 200 if PID resolves).
- **Cursor** (`/cursor/hook/...`) and **Codex** (`/codex/hook/...`): **synthetic PID** from `session_id` / Cursor payload fields — no `ConnectInfo` dependency; good for scripted curls.
- **Live state:** verify with **`coach status`** or **`curl http://127.0.0.1:$PORT/api/state`**. **`coach sessions list`** lists **saved transcript files** under projects — **not** the same as in-memory hook sessions (do not use it to validate HTTP tracking).
- **Malformed body:** non-JSON or empty POST body → **400** + axum JSON parse error text; **GET** on a hook route → **405**.
- **Codex/Cursor `{}`:** accepted (**200**, empty `{}` response) — synthetic PID for `"unknown"` / empty keys.
- **Concurrent hooks:** `xargs -P10` + multiple `UserPromptSubmit` on one Codex session — activity log shows all lines; **`event_count` stayed 0** (by design: only tool `record_tool` bumps it; see `state/mod.rs` tests).
- **Daemon restart:** in-memory sessions **cleared** on exit; after restart, **`/api/state` session count can still be ≥1** almost immediately because the **filesystem scanner** can attach a **real** local session (e.g. existing Claude Code window) — do not expect a stable “empty” baseline on dev machines.
- **Shell note:** spawning **many background `curl &` with `wait`** in one line sometimes wedged the tool runner; **`xargs -P`** was reliable for parallel POSTs.

## Daemon lifecycle (CLI, verified 2026-04-10)

Binary: `./target/release/coach` from repo root.

**Repro A — default port + status**

1. Ensure nothing listens on `7700` (or accept collision).
2. `cd .../coach && ./target/release/coach serve 2> /tmp/serve.err &` — wait ~1s.
3. **Startup (stderr):** `[coach serve] listening on 127.0.0.1:7700, priorities=[...]` (priorities from `~/.coach/settings.json`).
4. `./target/release/coach status` — exit 0; prints `port:    7700` and session summary from `GET /api/state`.
5. `kill %1` (or the recorded PID); confirm `lsof -iTCP:7700` empty.

**Repro B — custom `--port`**

1. `./target/release/coach serve --port 19991 2> /tmp/serve.err &`
2. Stderr: `[coach serve] listening on 127.0.0.1:19991, ...`
3. `coach status` still works: **`serve` writes the chosen port to `~/.coach/settings.json`** before listening, so `configured_port()` matches the daemon. There is **no** `coach config set port` — port changes go through `serve --port` (or editing the file).

**Repro C — port collision**

1. `./target/release/coach serve --port 19993 &` (wait until listening).
2. Second: `./target/release/coach serve --port 19993` — **exit 1**, stdout/stderr: `coach: failed to bind 127.0.0.1:19993: Address already in use (os error 48)` (macOS).
3. Kill the first process.

**Discrepancies / nits**

- **`coach status` when server down:** message says “Start the **GUI** first” — headless `serve` is enough; wording is slightly misleading.
- **`status` text output:** `model:` line shows extra quotes (`"openai" / "gpt-4.1-nano"`) because values are printed from JSON — cosmetic only.

## Discovery docs

- **`.kodo/test-report.md`** — setup commands, CLI verbatim help, smoke table, baselines **209 / 21** + **380** Vitest; artifact path `target/release/coach` at repo root — **accurate**.
- **`.kodo/test-coverage.md`** — Rust + Vitest baselines in that file; Codex/Cursor/Claude **HTTP** also covered by **2026-04-10 manual binary E2E** (see coverage rows).

## Optional / not run here

- **`tauri dev` / full GUI:** long-lived; blocked without display server.
- **`cargo test -- --ignored`:** needs API keys / external processes.
- **`pycoach` feature:** separate feature flag; not default.

## Misc

- `npm audit` advisories are separate from functional tests.
