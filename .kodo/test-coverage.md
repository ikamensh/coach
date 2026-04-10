# Feature Coverage

Tracked across `kodo test` runs.

**Baseline run**: 2026-04-10 — 591 pass (211 Rust + 380 Vitest), 0 fail, 21 ignored (see `test-report.md` for details)

**CLI E2E (config + path)**: 2026-04-10 — `target/release/coach`; Coach daemon **not** on port 7700 (file-backed `config set`); settings restored from backup after mutations.

**HTTP server E2E (binary, hooks + CLI)**: 2026-04-10 — `target/release/coach serve --port <PORT>` on localhost; **`curl`** to Claude `/hook/...`, Cursor `/cursor/hook/...`, Codex `/codex/hook/...`; verified with **`coach status`** + **`GET /api/state`**. **`coach sessions list`** exercised separately — lists **on-disk saved transcripts**, not hook server memory (see findings row).

## Test Entrypoints
- **TS**: `npm run test` → vitest, 36 files (380 tests) in `src/components/*.test.ts`
- **Rust**: `cargo test --workspace` → 211 tests across coach-core unit, cli_integration, hook_integration, scenario_replay
- **Ignored**: 21 tests need live API keys or external processes (see test-report.md)

| Feature / Workflow | Last tested | Status | Findings |
|--------------------|-------------|--------|----------|
| Dev: `cargo build --release` → `target/release/coach` | 2026-04-10 | pass | Workspace output dir is repo `target/`, not `src-tauri/target/` |
| Dev: `npm install` | 2026-04-10 | pass | `npm audit`: 1 high severity advisory |
| CLI: coach --version | 2026-04-10 | pass | Existing test `version_subcommand_does_not_start_tauri` |
| CLI: coach --help | 2026-04-10 | pass | Existing test `help_subcommand_does_not_start_tauri`; live text lists `hooks codex` + `hooks cursor` |
| CLI: coach serve --help | 2026-04-10 | **fixed** | Was starting daemon; now prints help and exits 0. Regression tests: `serve_help_does_not_start_daemon`, `wants_help_detects_flags` |
| CLI: coach serve (headless daemon) | 2026-04-10 | pass | Existing tests cover start, port collision, port release |
| CLI: coach hooks install/uninstall (Claude) | 2026-04-10 | pass | Existing test `cli_hooks_install_matches_install_hooks_at` |
| CLI: coach hooks codex status/install/uninstall | 2026-04-10 | pass | `check_codex_hook_status` / install paths in `cli.rs`; HTTP Codex routes in `server.rs` |
| CLI: coach hooks cursor install/uninstall | 2026-04-10 | pass | Existing test covers cursor hooks |
| CLI: coach config get/set | 2026-04-10 | pass | Unit/integration tests; see **E2E row** below |
| CLI E2E: `config get` / `get all` / keys | 2026-04-10 | pass | Full JSON includes `api_tokens` (empty here). Keys: priorities (numbered lines), `model`, `coach-mode` (**Debug:** `Llm`/`Rules`), `port`, `rules`. Unknown key → error exit 1 |
| CLI E2E: `config list` | 2026-04-10 | n/a | **Not a command** — `coach config list` → `usage: coach config <get|set>` |
| CLI E2E: `config set` + disk check | 2026-04-10 | pass | **Daemon down:** `set priorities`, `set model`, `set coach-mode`, `set rule outdated_models off/on`, `set api-token openai ""` (clears). Verified `~/.coach/settings.json` after sets; restored from backup |
| CLI E2E: invalid `config set` | 2026-04-10 | pass | Bad `coach-mode` / rule state → clear stderr + exit 1 |
| CLI E2E: `path status` | 2026-04-10 | pass | Default shim `~/.local/bin/coach` → App bundle; **`matches_running: false`** vs workspace binary; **`on $PATH: true`** |
| CLI E2E: `path install --dir <tmp>` | 2026-04-10 | pass | Symlink `…/coach` → `target/release/coach`; warns dir not on PATH; **`path status` unchanged** (reports default dir only) |
| CLI E2E: `path uninstall` | not run | n/a | Would remove **default** shim only — skipped to avoid touching user `~/.local/bin` |
| CLI: coach sessions list | 2026-04-10 | pass | Existing test handles missing projects dir |
| CLI: coach replay | 2026-04-10 | pass | Existing test for unknown session error |
| CLI: coach path install | 2026-04-10 | pass | Existing test creates shim |
| HTTP: /hook/post-tool-use | 2026-04-10 | pass | Creates session, tracks independently |
| HTTP: /hook/permission-request | 2026-04-10 | pass | Auto-approves in away mode |
| HTTP: /hook/stop | 2026-04-10 | pass | Blocks then allows on cooldown |
| HTTP: /hook/user-prompt-submit | 2026-04-10 | pass | Records activity, truncates long prompts |
| HTTP: Cursor hooks | 2026-04-10 | pass | cursor_after_shell_tracks_session |
| HTTP: Codex hooks (`/codex/hook/...`) | 2026-04-10 | **manual E2E pass** | Binary `serve` + `curl` post-tool-use, session-start, user-prompt-submit; `client: "codex"` in `/api/state`; integration tests still optional |
| HTTP E2E: malformed / wrong method | 2026-04-10 | pass | Bad JSON / empty POST → **400**; GET on hook path → **405** |
| HTTP E2E: empty JSON `{}` | 2026-04-10 | pass | Codex/Cursor: **200**; Claude `{}` resolves `session_id` to **`"unknown"`** |
| HTTP E2E: concurrent hooks | 2026-04-10 | pass | `xargs -P10` parallel Codex `user-prompt-submit` — activity entries match; `event_count` unchanged (tool-only counter) |
| HTTP E2E: daemon restart | 2026-04-10 | partial | Hook state not persisted (expected); fresh daemon may show **scanner-imported** sessions immediately — not an empty slate on active dev host |
| CLI vs HTTP: `sessions list` vs `status` | 2026-04-10 | **distinct** | `sessions list` = replay file index; **`status`** = live hook state |
| Session tracking: multiple sessions | 2026-04-10 | pass | multiple_sessions_tracked_independently |
| Session tracking: /clear replacement | 2026-04-10 | pass | clear_replaces_session_in_same_window |
| Session tracking: scanner discovery | 2026-04-10 | pass | scanner_discovers_real_sessions |
| Mode switching: present/away | 2026-04-10 | pass | API set mode tests |
| Rule engine: outdated models | 2026-04-10 | pass | post_tool_use_triggers_outdated_models_rule |
| LLM observer | 2026-04-10 | pass | observer_fires_in_llm_mode_and_records_failure |
| Settings: load/save | 2026-04-10 | pass | Unit tests for serde roundtrip |
| Hook installation: merge logic | 2026-04-10 | pass | install_hooks_at tested |
| Logging: file rotation | 2026-04-10 | pass | Unit tests for log rotation |
| PID resolution | 2026-04-10 | pass | Unit test resolves_real_connection_to_child_pid |
| Prompt loading | 2026-04-10 | pass | embedded_templates_are_non_empty |
| Frontend: React build (`npm run build`) | 2026-04-10 | pass | tsc + vite; ~1.3s |
| Frontend: ActivityBar component | 2026-04-10 | pass | Vitest |
| Frontend: SessionList component | 2026-04-10 | pass | Vitest |
| Frontend: SettingsPane component | 2026-04-10 | pass | Vitest |
| Frontend: type alignment with Rust | not tested | pending | types.ts vs Rust snapshots |
| GUI rendering | blocked | n/a | Requires display server |
| Real Claude Code integration | blocked | n/a | Requires claude CLI on PATH |
| Real Cursor integration | blocked | n/a | Requires cursor-agent on PATH |
| LLM observer with real API | blocked | n/a | Requires ANTHROPIC_API_KEY |
| UI smoke test | blocked | n/a | Requires macOS WindowServer |

## Appendix: CLI config / path E2E transcript (2026-04-10)

Binary: `/Users/ikamen/ai-workspace/ilya/coach/target/release/coach`. Shell: zsh on macOS. **`cp ~/.coach/settings.json /tmp/coach-settings.e2e.bak`** before mutations; **`cp` backup back** after.

```text
$ coach config list
coach: usage: coach config <get|set>; got 'list'
# exit 1

$ coach config get not_a_key
coach: unknown config key 'not_a_key'
# exit 1

$ coach config set priorities "E2E-first,E2E-second,E2E-third"
priorities = ["E2E-first", "E2E-second", "E2E-third"]

$ coach config get priorities
1. E2E-first
2. E2E-second
3. E2E-third

$ coach config set coach-mode rules
coach-mode = rules

$ coach config get coach-mode
Rules

$ coach config set coach-mode llm
coach-mode = llm

$ coach config get coach-mode
Llm

$ coach config set rule outdated_models off
rule outdated_models = off

$ coach path status
install path:    /Users/ikamen/.local/bin/coach
installed:       true
target:          /Applications/Coach.app/Contents/MacOS/coach
matches_running: false
on $PATH:        true

$ coach path install --dir /var/folders/.../tmp.XXXX
installed: /var/folders/.../tmp.XXXX/coach
target:    /Users/ikamen/ai-workspace/ilya/coach/target/release/coach
⚠  /var/folders/.../tmp.XXXX is not on $PATH.
```

## Appendix: HTTP hook server E2E (binary, 2026-04-10)

`PORT` was an ephemeral localhost port (e.g. 37882). **`./target/release/coach serve --port $PORT`** in background; **`coach status`** after (settings file updated to same `PORT`).

**Claude (lsof peer PID):**
```text
curl -sS -X POST "http://127.0.0.1:$PORT/hook/session-start" -H 'Content-Type: application/json' \
  -d '{"session_id":"e2e-claude-1","source":"startup","cwd":"/tmp/e2e-claude"}'
curl -sS -X POST "http://127.0.0.1:$PORT/hook/post-tool-use" -H 'Content-Type: application/json' \
  -d '{"session_id":"e2e-claude-1","tool_name":"Read","cwd":"/tmp/e2e-claude"}'
```

**Cursor (synthetic PID):**
```text
curl -sS -X POST "http://127.0.0.1:$PORT/cursor/hook/session-start" -H 'Content-Type: application/json' \
  -d '{"sessionId":"e2e-cursor-1","cwd":"/tmp/e2e-cursor"}'
curl -sS -X POST "http://127.0.0.1:$PORT/cursor/hook/after-shell" -H 'Content-Type: application/json' \
  -d '{"sessionId":"e2e-cursor-1","command":"pwd","cwd":"/tmp/e2e-cursor"}'
```

**Codex (synthetic PID):**
```text
curl -sS -X POST "http://127.0.0.1:$PORT/codex/hook/session-start" -H 'Content-Type: application/json' \
  -d '{"session_id":"e2e-codex-1","source":"startup","cwd":"/tmp/e2e-codex"}'
curl -sS -X POST "http://127.0.0.1:$PORT/codex/hook/post-tool-use" -H 'Content-Type: application/json' \
  -d '{"session_id":"e2e-codex-1","tool_name":"Write","cwd":"/tmp/e2e-codex"}'
```

**Verify:** `curl -sS "http://127.0.0.1:$PORT/api/state" | python3 -m json.tool` — expect `client` **claude** / **cursor** / **codex** on respective sessions. **`coach sessions list`** unchanged by these calls (different subsystem).
