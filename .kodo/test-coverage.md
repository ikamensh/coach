# Feature Coverage

Tracked across `kodo test` runs.

**Baseline run**: 2026-04-10 — **254** pass (**219** Rust + **35** Vitest), 0 fail, **21** ignored (`cargo test --workspace` + `npm test`). *(Earlier docs listed inflated Vitest counts when `vitest` picked up duplicate `*.test.ts` under **`.claude/worktrees/`**; `vite.config.ts` excludes that path.)* **CLI E2E re-verify:** `cargo build --release -p coach`; isolated `HOME` + `settings.json` `{"port":1}` — `coach status` down prints **`Start it with \`coach serve\` or launch the GUI`**; `coach serve --port` then `coach status` exit 0; **`path status|uninstall --dir`** custom-dir roundtrip. **Stage 5 re-verify:** `npm test` green; `cargo test --workspace` **219** passed / **21** ignored.

**CLI E2E (config + path)**: 2026-04-10 — `target/release/coach`; Coach daemon **not** on port 7700 (file-backed `config set`); settings restored from backup after mutations.

**HTTP server E2E (binary, hooks + CLI)**: 2026-04-10 — `target/release/coach serve --port <PORT>` on localhost; **`curl`** to Claude `/hook/...`, Cursor `/cursor/hook/...`, Codex `/codex/hook/...`; verified with **`coach status`** + **`GET /api/state`**. **`coach sessions list`** exercised separately — lists **on-disk saved transcripts**, not hook server memory (see findings row).

## Test Entrypoints
- **TS**: `npm run test` → vitest, **3** files (**35** tests): `ActivityBar.test.ts`, `SessionList.test.ts`, `SettingsPane.test.ts` under `src/components/`. Vitest config excludes **`**/.claude/**`** so Claude Code worktrees do not duplicate tests.
- **Rust**: `cargo test --workspace` → **219** tests (coach-core, hook_integration, cli_integration, scenario_replay, etc.), **21** ignored
- **Ignored**: 21 Rust tests need live API keys or external processes (see test-report.md)

## Stage 5 — Frontend (build + Vitest)

**`package.json` scripts relevant to validation:** `build` (`tsc -b tsconfig.app.json && vite build`), `test` (`vitest run`), `dev` (Vite only — no Tauri). There is **no** `lint` script; type safety is **`tsc`** via `build`.

**Test layout (repo):** only **`src/components/ActivityBar.test.ts`**, **`SessionList.test.ts`**, **`SettingsPane.test.ts`**. Vitest **`test.exclude`** in `vite.config.ts` includes **`**/.claude/**`** so duplicate trees under `.claude/worktrees/` are not collected.

### Stage 5 re-check (2026-04-10, tester session — provider-capability UX)

| Step | Result | Notes |
|------|--------|--------|
| `npm run build` | **pass** | `tsc -b` + Vite **~1.8s**; `dist/assets/index-vb_cAP9x.css` ~30.67 kB gzip ~5.98 kB; `index-BXCzk39j.js` ~250 kB gzip ~74.42 kB |
| `npm test` | **pass** | **3** files, **35** tests, **~150ms** (Vitest 4.1.2) |

**What Vitest actually runs (narrow scope — no component mount, no Tauri):**

- **`ActivityBar.test.ts`**: imports **`activityOpacity`**, **`activityColor`** from `ActivityBar.tsx` (pure helpers).
- **`SessionList.test.ts`**: **`topTools`** from `SessionList.tsx`; **`timeAgo`**, **`formatDuration`** from `utils/time.ts`.
- **`SettingsPane.test.ts`**: **`PROVIDERS`** shape tests + **`observer-capable provider consistency`** vs duplicated **`BACKEND_OBSERVER_CAPABLE`** (mirrors `coach-core` `OBSERVER_CAPABLE_PROVIDERS`) — **not** a mounted `SettingsPane`, not `invoke` / `get_state`.

**Implemented UX (code review + build; GUI not launched):** `CoachSnapshot` in **`src/types.ts`** includes **`observer_capable_providers`**. **`useCoachStore`** hydrates **`observerCapableProviders`** from **`get_state`** and **`coach-state-updated`**. **`SettingsPane`** labels non-capable providers **`(no observer)`** in the provider `<select>` and shows an amber warning (`data-testid="observer-warning"`) when **LLM** engine mode is selected and the current provider is not in the server list.

**Not covered by `npm test` / `npm run build`:** full webview/Tauri runtime (`invoke`/`listen` integration), DOM visibility of the warning. **`tests/test_ui_smoke.py`** remains separate / manual.

### Backend vs Settings UI — observer-capable providers (tester E2E, HTTP)

**Source of truth (Rust):** `coach-core/src/settings/mod.rs` — `OBSERVER_CAPABLE_PROVIDERS` = **`openai`**, **`anthropic`**, **`google`**. **`openrouter` is not** observer-capable.

**Settings dropdown (`SettingsPane.tsx`):** four static providers; options append **`(no observer)`** when `id` ∉ **`observer_capable_providers`** from snapshot. With **LLM** engine + non-capable provider, an amber line explains observer sessions are unavailable (`data-testid="observer-warning"`).

**Alignment:** Backend and static catalog **agree on which providers exist**. **OpenRouter** is valid for rules / one-shot paths but **will not** run the accumulating observer chain (see `hook_integration::observer_does_not_fire_for_non_capable_provider`). **Google** is observer-capable in Rust. The Settings UI now surfaces capability via **`observer_capable_providers`** from state (not a hard-coded-only guess).

**HTTP repro (release binary, ephemeral port):**

1. `./target/release/coach serve --port 37891 &` — wait until listening.
2. `curl -sS http://127.0.0.1:37891/api/state` — JSON includes `"observer_capable_providers":["openai","anthropic","google"]`. Same payload from `GET /state` (same handler).
3. `curl -sS -X POST http://127.0.0.1:37891/api/config/model -H 'Content-Type: application/json' -d '{"provider":"openrouter","model":"qwen/qwen3.5-397b-a17b"}'` — **HTTP 200**, full snapshot; **`observer_capable_providers` unchanged**; **`model`** switches to OpenRouter (persists to **`~/.coach/settings.json`** — restore with `coach config set model <provider> <model>` if you did not intend to change the real profile).
4. `kill` the `serve` process when done.

**Stage 5 re-check finding:** build + Vitest green; Vitest asserts **PROVIDERS** ⊇ backend observer-capable set and that at least one provider is non-observer (OpenRouter). **Tauri GUI** not run in this pass (`tauri dev` / webview) — warning/`(no observer)` labels not visually confirmed.

| Feature / Workflow | Last tested | Status | Findings |
|--------------------|-------------|--------|----------|
| Dev: `cargo build --release` → `target/release/coach` | 2026-04-10 | pass | `cargo build --release -p coach`; workspace `target/` at repo root |
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
| CLI E2E: `path install --dir <tmp>` | 2026-04-10 | pass | Symlink `…/coach` → release binary; warns dir not on PATH |
| CLI E2E: `path status --dir` / `path uninstall --dir` | 2026-04-10 | pass | Custom install dir: **`path status --dir`** reports that shim; **`path uninstall --dir`** removes it (default-dir **`path uninstall`** unchanged). Integration: **`cli_path_install_then_uninstall_roundtrip_with_custom_dir`** |
| CLI E2E: `path uninstall` (default dir) | 2026-04-10 | pass | Removes **`$HOME/.local/bin/coach`** only. Second uninstall when missing → **exit 1** |
| CLI E2E: `coach status` + headless `serve` | 2026-04-10 | pass | Isolated **`HOME`** + **`settings.json` `port`** on a **free** port: no daemon → **exit 1**; error text includes **`coach serve`** then **GUI**. **`coach serve --port`** → **`status` / `status --json`** exit **0**; after kill serve → **exit 1**. **Trap:** empty isolated `HOME` defaults port **7700** — may hit another running Coach on the host |
| CLI E2E: `coach status --json` daemon down | 2026-04-10 | **behavior note** | **`--json` does not** emit JSON on failure — same plain-text error as non-JSON; exit **1** |
| CLI E2E: hooks merge + uninstall (Claude + Cursor, binary) | 2026-04-10 | pass | **`HOME=$(mktemp -d)`**, **`target/release/coach`**. Valid dirty configs: **`shasum -a 256`** stable on **2nd** `hooks install` / `hooks cursor install`; uninstall preserves unrelated keys + user hooks. **Malformed:** syntax-invalid → **install exit 1**, file **unchanged**, **no shim**; uninstall → **exit 1**, file kept. Valid non-object root / `hooks` not object → **install exit 1**, file kept. See appendix **Hook E2E (dirty + malformed)** |
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
| Settings: corrupt/malformed JSON recovery (CLI + `serve`, release binary) | 2026-04-10 | pass | Truncated / non-JSON / empty / wrong-type → warning + defaults; `{}` silent defaults; read-only leaves file corrupt; `config set` + `serve` rewrite file. **Data loss** of prior fields on recovery without backup; see `tester-notes` |
| Hook installation: merge logic | 2026-04-10 | pass | install_hooks_at tested |
| Logging: file rotation | 2026-04-10 | pass | Unit tests for log rotation |
| PID resolution | 2026-04-10 | pass | Unit test resolves_real_connection_to_child_pid |
| Prompt loading | 2026-04-10 | pass | embedded_templates_are_non_empty |
| Frontend: React build (`npm run build`) | 2026-04-10 | pass | tsc + vite; ~1.3–1.5s (see **Stage 5** row) |
| Frontend: ActivityBar helpers | 2026-04-10 | pass | Vitest (`activityOpacity` / `activityColor`), not full component render |
| Frontend: SessionList + `utils/time` | 2026-04-10 | pass | Vitest (`topTools`, `timeAgo`, `formatDuration`) |
| Frontend: `PROVIDERS` + observer-capable consistency (`SettingsPane.test.ts`) | 2026-04-10 | pass | Shape + backend list alignment tests — not mounted Settings tree |
| Frontend: Vitest scope | 2026-04-10 | **35 tests / 3 files** | `vite.config.ts` excludes `**/.claude/**` |
| HTTP E2E: `observer_capable_providers` + model POST | 2026-04-10 | pass | `GET /api/state` / `GET /state`: `["openai","anthropic","google"]`; `POST /api/config/model` **`openrouter`** — same list. Settings UI reads list from IPC snapshot (**`src/types.ts`** includes field) |
| Frontend: type alignment with Rust | 2026-04-10 | partial | **`observer_capable_providers`** on TS `CoachSnapshot`; no automated serde roundtrip test front↔back |
| Frontend: Tauri / Settings UX | 2026-04-10 | partial | Logic wired in store + `SettingsPane`; **no** `tauri dev` / webview pass this run |
| **Linux ARM64: `npm run build`** | 2026-04-10 | **pass** | Debian 12, Node 22.22.0, `tsc -b` + Vite **~1.56s**; `dist/assets/index-DRR5y358.css` ~30.65 kB, `index-ByFNAYfX.js` ~250.58 kB |
| **Linux ARM64: `cargo build --release -p coach`** | 2026-04-10 | **pass (fixed)** | Debian 12, Rust 1.94.1, aarch64. Initial failure: `RunEvent::Reopen` is macOS-only — fixed with `#[cfg(target_os = "macos")]`. Clean build after fix (~42s first, ~29s incremental). Zero warnings. |
| **Linux ARM64: `coach --version`** | 2026-04-10 | **pass** | `coach 0.1.76` — matches workspace version |
| **Linux ARM64: `coach --help`** | 2026-04-10 | **pass** | Full CLI usage printed correctly |
| **Linux ARM64: `cargo test --workspace`** | 2026-04-10 | **pass** | **219 passed**, 0 failed, **21 ignored** (identical to macOS) |
| **Linux ARM64: `npm test`** | 2026-04-10 | **pass** | **35 tests**, 3 files, ~489ms (Vitest 4.1.2) |
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

## Appendix: PATH shim + Claude/Cursor hook files (binary E2E, 2026-04-10)

**Goal:** Safe E2E without touching the real home directory.

### A — Dirty valid configs + idempotency + selective uninstall

1. `export HOME="$(mktemp -d)"`; `mkdir -p "$HOME/.coach" "$HOME/.claude" "$HOME/.cursor"`.
2. Seed **`$HOME/.claude/settings.json`** with e.g. `"someUserSetting"`, `"permissions"`, and extra **`hooks.PostToolUse`** / **`SessionStart`** entries using nested `{"hooks":[{"type":"command","command":"…"}]}` (non-Coach commands).
3. Seed **`$HOME/.cursor/hooks.json`** with `"version":1`, a custom top-level key (e.g. **`gleanerMeta`**), and user **`hooks.afterShellExecution`** / **`sessionStart`** `command` arrays.
4. From repo root: **`./target/release/coach path install`** (optional repeat — idempotent shim).
5. **`./target/release/coach hooks install`** twice — `shasum -a 256 "$HOME/.claude/settings.json"` **unchanged** on second run.
6. **`./target/release/coach hooks cursor install`** twice — `shasum -a 256 "$HOME/.cursor/hooks.json"` **unchanged** on second run.
7. **`./target/release/coach hooks uninstall`** then **`hooks cursor uninstall`** — confirm unrelated keys and user hook commands remain; Coach shim files removed from **`$HOME/.coach/`** and **`$HOME/.cursor/`**.
8. **`./target/release/coach path uninstall`** — removes **`$HOME/.local/bin/coach`** only.

### B — Malformed files (same binary, isolated `HOME`)

| Precondition | Command | Expected |
|--------------|---------|----------|
| Claude `settings.json` = `{ "almost":"json` (invalid) | `hooks install` | Exit **1**, **`refusing to overwrite … invalid JSON`**; file **unchanged**; **no** `~/.coach/claude-hook.sh` |
| Claude `settings.json` = `[1,2,3]` | `hooks install` | Exit **1**, message **`config file is not a JSON object`**, file **unchanged** |
| Claude `{"hooks":"nope","keep":true}` | `hooks install` | Exit **1**, **`hooks is not an object`**, file **unchanged** |
| Cursor `hooks.json` = `not json at all` | `hooks cursor install` | Exit **1**, **`refusing to overwrite … invalid JSON`**; file **unchanged**; **no** `coach-cursor-hook.sh` |
| Any unparseable `settings.json` / `hooks.json` | `hooks uninstall` / `hooks cursor uninstall` | Exit **1**, serde error on stderr, **file unchanged** |

**Custom install dir:** use **`path uninstall --dir`** to remove a shim from **`path install --dir`**; verify with **`path status --dir`** (see integration test **`cli_path_install_then_uninstall_roundtrip_with_custom_dir`**).
