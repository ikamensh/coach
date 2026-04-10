# Feature Coverage

Tracked across `kodo test` runs.

**Baseline run**: 2026-04-10 — 589 pass, 0 fail, 21 ignored (see `test-report.md` for details)

**CLI E2E (config + path)**: 2026-04-10 — `target/release/coach`; Coach daemon **not** on port 7700 (file-backed `config set`); settings restored from backup after mutations.

## Test Entrypoints
- **TS**: `npm run test` → vitest, 36 files (380 tests) in `src/components/*.test.ts`
- **Rust**: `cargo test --workspace` → 209 tests across coach-core unit, cli_integration, hook_integration, scenario_replay
- **Ignored**: 21 tests need live API keys or external processes (see test-report.md)

| Feature / Workflow | Last tested | Status | Findings |
|--------------------|-------------|--------|----------|
| Dev: `cargo build --release` → `target/release/coach` | 2026-04-10 | pass | Workspace output dir is repo `target/`, not `src-tauri/target/` |
| Dev: `npm install` | 2026-04-10 | pass | `npm audit`: 1 high severity advisory |
| CLI: coach --version | 2026-04-10 | pass | Existing test `version_subcommand_does_not_start_tauri` |
| CLI: coach --help | 2026-04-10 | pass | Existing test `help_subcommand_does_not_start_tauri`; live text lists `hooks codex` + `hooks cursor` |
| CLI: coach serve --help | 2026-04-10 | quirk | Does **not** show help — starts headless daemon (`serve` only parses `--port`) |
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
| HTTP: Codex hooks (`/codex/hook/...`) | not tested | pending | Routes exist in `server.rs`; no dedicated hook_integration test found |
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
