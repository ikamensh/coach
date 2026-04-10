# Coach — tester notes

## Environment (verified 2026-04-10)

- **Binary:** Workspace outputs `coach` at **`/coach/target/release/coach`** (or `target/debug/coach` after `cargo test`). Not under `src-tauri/target/` alone — use workspace root `target/`.
- **Rust:** `cargo test --workspace` — **211** passed, **21** ignored (e.g. coach-core 164+15, hook_integration 29+2, cli 17, scenario_replay 1+4). Use `--workspace`; bare `cargo test` from repo root can be misleading if you only read the last `test result` line.
- **Node:** `npm test` — Vitest **380** tests, 36 files. `npm run build` — green (~1.5s Vite).
- **CLI:** `./target/release/coach --version` matches workspace (e.g. `0.1.74`).

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

## External hooks + PATH shim (binary E2E, tightened 2026-04-10)

Isolate with **`export HOME="$(mktemp -d)"`** — `~/.claude/settings.json`, `~/.cursor/hooks.json`, `~/.coach/`, **`~/.local/bin/coach`** all under that tree.

**Dirty but valid JSON (realistic pre-existing configs):** `coach path install` then **`coach hooks install`** twice — second run leaves **`settings.json` SHA-256 unchanged**; **`coach hooks cursor install`** twice — second run leaves **`hooks.json` SHA-256 unchanged**. `hooks uninstall` / **`hooks cursor uninstall`** remove only Coach-managed hook entries and shim scripts (`~/.coach/claude-hook.sh`, `~/.cursor/coach-cursor-hook.sh`); unrelated top-level keys (`someUserSetting`, `permissions`), nested non-Coach Claude `command` hooks, and Cursor extras (`gleanerMeta`, user `command` rows) **remain**.

**Malformed / edge cases (`install_nested_hooks` / `install_cursor_hooks_at` in `hooks.rs`):**

| Case | `hooks install` / `hooks cursor install` | `hooks uninstall` / `hooks cursor uninstall` |
|------|--------------------------------------------|---------------------------------------------|
| **Syntax-invalid JSON** (truncated `{`, `not json`) | **Exit 1** — `refusing to overwrite … — it contains invalid JSON: …`; **config file bytes unchanged**; **no new shim** (`~/.coach/claude-hook.sh` / `~/.cursor/coach-cursor-hook.sh`) — parse runs **before** any shim write (**E2E 2026-04-10**, `target/release/coach` **0.1.74**, isolated `HOME`). | **Exit 1** — parse error; **file unchanged** |
| **Valid JSON, root not an object** (e.g. `[1,2,3]`) | **Exit 1** — `config file is not a JSON object` (Claude); **file unchanged** | Same — **unchanged** if still invalid |
| **Root object but `"hooks"` not an object** (e.g. `"hooks":"nope"`) | **Exit 1** — `hooks is not an object`; **file unchanged** | Needs parseable `hooks` object — **fails** if still wrong |

**Gaps / nits**

- **`path uninstall` has no `--dir`.** Custom-dir shim from **`path install --dir`** survives **`path uninstall`** (only default `~/.local/bin/coach`). **Reconfirmed 2026-04-10** vs **`target/release/coach` 0.1.74**: after `path install` + `path install --dir $CUSTOM`, `path uninstall` removes only `$HOME/.local/bin/coach`; **`$CUSTOM/coach` symlink remains** (see Stage 4 E2E report).
- **Success messages** show literal `~/.claude/…` while **`HOME` override** shows different paths on disk (cosmetic).

## Stage 4 — syntax-invalid hook JSON (fixed 2026-04-10)

- **Previous bug:** parse failure was treated as empty `{}` → install **overwrote** corrupt JSON with valid merged output (**exit 0**).
- **Current behavior:** `install_nested_hooks` / `install_cursor_hooks_at` validate existing JSON **before** writing shims or configs — **exit 1**, stderr cites parse error; **config bytes preserved**; **no orphaned shims** on that failure path.
- **Regression E2E:** `cargo build --release -p coach` then isolated `HOME` + invalid configs — **exit 1**, SHA unchanged, no `~/.coach` (Claude case) / no `coach-cursor-hook.sh` (Cursor case); fresh `HOME` first install — **exit 0**, both JSON files and both shims created.

## Discovery docs

- **`.kodo/test-report.md`** — setup commands, CLI verbatim help, smoke table, baselines **209 / 21** + **380** Vitest; artifact path `target/release/coach` at repo root — **accurate**. **Note:** its `serve --help` quirk is **fixed** in current tree (see `test-coverage.md`).
- **`.kodo/test-coverage.md`** — Rust + Vitest baselines in that file; Codex/Cursor/Claude **HTTP** also covered by **2026-04-10 manual binary E2E** (see coverage rows).

## Optional / not run here

- **`tauri dev` / full GUI:** long-lived; blocked without display server.
- **`cargo test -- --ignored`:** needs API keys / external processes.
- **`pycoach` feature:** separate feature flag; not default.

## Settings file corruption (`~/.coach/settings.json`, E2E 2026-04-10)

- **Binary:** `coach/target/release/coach` (0.1.74).
- **Implementation:** `Settings::load_from` — parse error → `eprintln!` warning + **`Settings::default()`**; missing file → defaults **without** warning (`read` error).
- **Read-only (`config get`, etc.):** corrupt file **stays on disk** unchanged; stderr shows serde error; values shown are **full defaults** (not a partial merge).
- **`{}`:** valid JSON — deserializes with serde defaults for missing fields → **no warning** (differs from syntax-invalid files).
- **Repair (writes valid JSON):** `coach serve --port P` (saves after load); `coach config set …` with **no** daemon on `configured_port()` (file path); `coach config set …` with daemon up (HTTP → `CoachState::save()`). Any save **replaces the whole file** — prior settings that only existed in the corrupt blob are **not recoverable** unless the user kept a backup.
- **Nits:** `config set` with corrupt file can **print the parse warning twice** (`configured_port()` + inner `Settings::load()`). While corrupt, **`configured_port()` is 7700**, so `hooks install` / server probe use default port — can mismatch a custom port that was only in the broken file.

## Misc

- `npm audit` advisories are separate from functional tests.
