# Coach — tester notes

## Environment (verified 2026-04-10)

- **Binary:** Workspace outputs `coach` at **`target/release/coach`** from repo root (or `target/debug/coach` after `cargo test`). Not under `src-tauri/target/` alone — use workspace root `target/`. **0.1.78** in this pass.
- **Rust:** `cargo test --workspace` — **219** passed, **21** ignored (2026-04-10 re-verify). Use `--workspace`; bare `cargo test` from repo root can be misleading if you only read the last `test result` line. Per-crate spot check: `hook_integration` **31** tests (not 29); `coach-core` unit **171** passed + **15** ignored.
- **Node:** **`npm run build`** = `tsc -b tsconfig.app.json && vite build`; **`npm test`** = `vitest run`. Vitest **35** tests, **3** files — **`ActivityBar`**, **`SessionList`**, **`SettingsPane`** `*.test.ts` (helpers + **`PROVIDERS`** + observer-capable consistency vs duplicated backend list; **no** full `SettingsPane` mount, **no** Tauri). **`vite.config.ts`** excludes **`**/.claude/**`**. No ESLint script.
- **CLI:** `./target/release/coach --version` matches workspace (e.g. `0.1.78`).

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

## Daemon lifecycle + `coach status` (CLI, verified 2026-04-10)

Binary: `./target/release/coach` from repo root.

**Isolation gotcha:** Fresh `HOME` with **no** `~/.coach/settings.json` uses **default port 7700**. If anything already listens there (another Coach/GUI on the same machine), **`coach status` succeeds against that process** — not a “no server” test. For a controlled offline check, seed **`~/.coach/settings.json`** with `{"port": N}` where **N** is a free localhost port, then expect **exit 1** until `serve --port N` runs.

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
2. Second: `./target/release/coach serve --port 19993` — **exit 1**, stdout/stderr: `coach: failed to bind 127.0.0.1:19993: Address already in use (os error 48)` (**macOS**) or **`os error 98`** (**Linux**).
3. Kill the first process.

**`status --json` (E2E):** When the daemon is **up**, output is pretty-printed JSON (exit 0). When **down**, stderr/stdout is still the **plain text** error (`Coach is not running on port …`) — **not** JSON — exit **1**. Scripts cannot rely on `--json` alone for machine-readable errors.

**Wording (daemon down):** Error is **`Coach is not running on port N. Start it with \`coach serve\` or launch the GUI.`** — mentions headless **`serve`** first; GUI remains as alternative.

**Nits**

- **`status` text output:** `model:` line shows extra quotes (`"openai" / "gpt-4.1-nano"`) because values are printed from JSON — cosmetic only.

## External hooks + PATH shim (binary E2E, tightened 2026-04-10)

Isolate with **`export HOME="$(mktemp -d)"`** — `~/.claude/settings.json`, `~/.cursor/hooks.json`, `~/.coach/`, **`~/.local/bin/coach`** all under that tree.

**Dirty but valid JSON (realistic pre-existing configs):** `coach path install` then **`coach hooks install`** twice — second run leaves **`settings.json` SHA-256 unchanged**; **`coach hooks cursor install`** twice — second run leaves **`hooks.json` SHA-256 unchanged**. `hooks uninstall` / **`hooks cursor uninstall`** remove only Coach-managed hook entries and shim scripts (`~/.coach/claude-hook.sh`, `~/.cursor/coach-cursor-hook.sh`); unrelated top-level keys (`someUserSetting`, `permissions`), nested non-Coach Claude `command` hooks, and Cursor extras (`gleanerMeta`, user `command` rows) **remain**.

**Malformed / edge cases (`install_nested_hooks` / `install_cursor_hooks_at` in `hooks.rs`):**

| Case | `hooks install` / `hooks cursor install` | `hooks uninstall` / `hooks cursor uninstall` |
|------|--------------------------------------------|---------------------------------------------|
| **Syntax-invalid JSON** (truncated `{`, `not json`) | **Exit 1** — `refusing to overwrite … — it contains invalid JSON: …`; **config file bytes unchanged**; **no new shim** (`~/.coach/claude-hook.sh` / `~/.cursor/coach-cursor-hook.sh`) — parse runs **before** any shim write (**E2E 2026-04-10**, `target/release/coach` **0.1.78**, isolated `HOME`). | **Exit 1** — parse error; **file unchanged** |
| **Valid JSON, root not an object** (e.g. `[1,2,3]`) | **Exit 1** — `config file is not a JSON object` (Claude); **file unchanged** | Same — **unchanged** if still invalid |
| **Root object but `"hooks"` not an object** (e.g. `"hooks":"nope"`) | **Exit 1** — `hooks is not an object`; **file unchanged** | Needs parseable `hooks` object — **fails** if still wrong |

**Gaps / nits**

- **`path uninstall` without `--dir`** still removes only the default shim (`~/.local/bin/coach`). For a custom install dir, use **`path uninstall --dir <DIR>`** (pairs with **`path install --dir`** / **`path status --dir`**). **E2E 2026-04-10:** release binary + isolated `HOME` — custom-dir roundtrip verified; integration test `cli_path_install_then_uninstall_roundtrip_with_custom_dir`.
- **Success messages** show literal `~/.claude/…` while **`HOME` override** shows different paths on disk (cosmetic).

## Stage 4 — syntax-invalid hook JSON (fixed 2026-04-10)

- **Previous bug:** parse failure was treated as empty `{}` → install **overwrote** corrupt JSON with valid merged output (**exit 0**).
- **Current behavior:** `install_nested_hooks` / `install_cursor_hooks_at` validate existing JSON **before** writing shims or configs — **exit 1**, stderr cites parse error; **config bytes preserved**; **no orphaned shims** on that failure path.
- **Regression E2E:** `cargo build --release -p coach` then isolated `HOME` + invalid configs — **exit 1**, SHA unchanged, no `~/.coach` (Claude case) / no `coach-cursor-hook.sh` (Cursor case); fresh `HOME` first install — **exit 0**, both JSON files and both shims created.

## Stage 5 — frontend (Vitest + build, re-verified 2026-04-10)

- **`npm run build`**: pass — `tsc -b tsconfig.app.json` + Vite **~1.8s**; `dist/` (`assets/index-*.js` ~250 kB gzip ~74 kB).
- **`npm test`**: pass — **3** files, **35** tests, **~150ms** (Vitest **4.1.2**).
- **Lint / typecheck scripts:** **`package.json` has no `lint`**. Typecheck is **`tsc -b`** inside **`npm run build`** only (no standalone `typecheck` script).
- **Rust `cargo test --workspace`**: **219** passed, **21** ignored (same baseline as notes).
- **IPC audit (commands):** `rg` on `src/**/*.ts(x)` `invoke("…")` vs `generate_handler![…]` in `src-tauri/src/lib.rs` — **26** Tauri commands; **set equality match** with all `invoke` sites (**`useCoachStore.ts`** + **`DevPane.tsx`**). No Rust-only or frontend-only names.
- **IPC audit (events):** Rust emits `coach_core::state::EVENT_STATE_UPDATED` / `EVENT_THEME_CHANGED` (`coach-state-updated`, `coach-theme-changed`). **`useCoachStore.ts`** listens on the same two strings — **match**. (Tray/commands also emit state updates — same event name.)
- **Provider / observer UX (worker fix):** **`CoachSnapshot`** includes **`observer_capable_providers`**; store hydrates from **`get_state`** + **`coach-state-updated`**. **`SettingsPane`**: provider `<select>` appends **`(no observer)`** when not in that list; **LLM** mode + non-capable provider shows amber warning (`data-testid="observer-warning"`). Vitest covers **PROVIDERS** ⊇ backend observer list + non-observer providers have labels — **not** mounted UI / webview.
- **Real scope:** Still **no** `tauri dev` in automated pass — runtime `invoke`/`listen` and DOM visibility of warnings not exercised here.
- **HTTP E2E (binary):** `GET /api/state` / **`GET /state`** include **`observer_capable_providers`**. **`POST /api/config/model`** **`openrouter`** accepted; list unchanged. For IPC parity, same field is on the Tauri snapshot type.

## Discovery docs

- **`.kodo/test-report.md`** — may list older Vitest/Rust totals; prefer **`test-coverage.md`** for current **35** Vitest / **219** Rust numbers.
- **`.kodo/test-coverage.md`** — Rust + Vitest + Stage 5 frontend; HTTP hook E2E in coverage table.

## Optional / not run here

- **`tauri dev` / full GUI:** long-lived; blocked without display server.
- **`cargo test -- --ignored`:** needs API keys / external processes.
- **`pycoach` feature:** separate feature flag; not default.

## Settings file corruption (`~/.coach/settings.json`, E2E 2026-04-10)

- **Binary:** `coach/target/release/coach` (0.1.78).
- **Implementation:** `Settings::load_from` — parse error → `eprintln!` warning + **`Settings::default()`**; missing file → defaults **without** warning (`read` error).
- **Read-only (`config get`, etc.):** corrupt file **stays on disk** unchanged; stderr shows serde error; values shown are **full defaults** (not a partial merge).
- **`{}`:** valid JSON — deserializes with serde defaults for missing fields → **no warning** (differs from syntax-invalid files).
- **Repair (writes valid JSON):** `coach serve --port P` (saves after load); `coach config set …` with **no** daemon on `configured_port()` (file path); `coach config set …` with daemon up (HTTP → `CoachState::save()`). Any save **replaces the whole file** — prior settings that only existed in the corrupt blob are **not recoverable** unless the user kept a backup.
- **Nits:** `config set` with corrupt file can **print the parse warning twice** (`configured_port()` + inner `Settings::load()`). While corrupt, **`configured_port()` is 7700**, so `hooks install` / server probe use default port — can mismatch a custom port that was only in the broken file.

## Linux ARM64 — Debian 12 VPS

### Release binary quick E2E (2026-04-10, re-run)

**Host:** `root@46.225.111.102`. **`env -i` + `mktemp` `HOME`**, seeded `~/.coach/settings.json` `port` → **`coach status`** exit **1** when down, **`coach serve --port`** + **`coach status`** + **`curl /api/state`** OK, second **`serve`** → **EADDRINUSE `os error 98`**. **`pid_resolver::tests::resolves_real_connection_to_child_pid`** — `cargo test -p coach-core … -- --exact` **pass** (uses **`/proc/net/tcp`** / netstat-style path on Linux).

### Stage 2 verification (2026-04-10, re-run)

**Host:** `root@46.225.111.102` (`openclaw-1`), **6.1.0-44-arm64**, Rust **1.94.1**. Sync: **`rsync -avz --delete`** excluding `target/`, `node_modules/`, `.claude/`, `dist/`, `.git/` (include `.kodo/` if you want notes parity).

**Authoritative clean env (no API keys leaking from login shell):**

```bash
ssh root@46.225.111.102 'bash -lc "cd /root/coach && export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin && env -i HOME=/root USER=root PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin RUST_BACKTRACE=1 env -u OPENAI_API_KEY -u ANTHROPIC_API_KEY -u GOOGLE_API_KEY -u GEMINI_API_KEY -u OPENROUTER_API_KEY cargo test --workspace"'
```

**`cargo test --workspace`:** **219** passed, **0** failed, **21** ignored — per-crate: `coach_lib` **0**; `coach` **0**; `cli_integration` **18**; `pycoach_sidecar` **0**; coach-core unit **171** + **15** ign; `hook_integration` **29** + **2** ign; `scenario_replay` **1** + **4** ign; doc-tests **0**.

**`observer_does_not_fire_in_rules_mode` — wrong test setup, not a product bug.** Production gates the observer queue on `coach_mode == Llm` + capable provider (`server.rs` `run_post_tool_use`). The test must set **`coach_mode = EngineMode::Rules`** because `Settings::default()` is **Llm**. Extra check: **`OPENAI_API_KEY=sk-fake… cargo test -p coach-core observer_does_not_fire_in_rules_mode -- --exact`** still **pass** — confirms Rules mode, not missing keys/timing.

**Linux-specific checks:** **`pid_resolver::tests::resolves_real_connection_to_child_pid`** (**netstat2** / **`/proc/net/tcp`**) — **pass** in suite; run alone: **`cargo test -p coach-core pid_resolver::tests::resolves_real_connection_to_child_pid -- --exact`**.

**`npm test`:** **35** passed (3 files) — not re-run this session; prior baseline unchanged unless `package.json` shifts.

### Stage 3 — release binary: `~/.local/bin` shim + hooks (Debian 12 ARM64 VPS)

**Host:** `root@46.225.111.102` (**openclaw-1**, **6.1.0-44-arm64**). **Binary under test:** **`/root/coach/target/release/coach`** (workspace **`cargo build --release -p coach`** on the VPS — do not use stale packaged artifacts). **Re-verified:** **2026-04-10** — **`coach 0.1.78`**, **`ELF … ARM aarch64`**.

**Shell rc / PATH — product behavior:** Coach **does not** create or edit **`~/.bashrc`**, **`~/.profile`**, or **`~/.zshrc`**. It only **detects** whether the install directory is on **`$PATH`** (`path_install::dir_on_path`) and, if not, **prints** **`export PATH="<dir>:$PATH"`** (stdout). Verified with **`env -i HOME=$(mktemp -d)`**.

**Isolation:** Every scenario uses **`env -i HOME=<tmp> USER=test PATH=…`** so Claude (**`~/.claude/settings.json`**), Cursor (**`~/.cursor/hooks.json`**), Codex (**`~/.codex/hooks.json`**), and shims under **`~/.coach/`** stay separate from the real admin **`HOME`**.

**PATH shim (same results as prior pass):** **`path install`** without `~/.local/bin` on **`PATH`** → warning + symlink; **`path status`** → **`on $PATH: true`** when prepended; **`path install`** twice → idempotent; **`path uninstall`** ×2 → second **exit non-zero** (`no shim installed at …`); **`path install --dir` / `status --dir` / `uninstall --dir`** roundtrip OK.

**Hooks — Claude / Cursor / Codex (dirty pre-existing JSON):** User keys + extra hook rows preserved; **`hooks install`** / **`hooks cursor install`** / **`hooks codex install`** each run **3×** — config **`sha256sum`** stable after the **2nd** run (idempotent); shims **`claude-hook.sh`**, **`coach-cursor-hook.sh`**, **`codex-hook.sh`** executable; **`hooks <target> status`** → **`all installed: true`**; **`hooks uninstall`** + **`hooks cursor uninstall`** + **`hooks codex uninstall`** → Coach shims removed, user JSON content retained.

**Missing dirs:** Fresh **`HOME`** with only **`~/.coach/settings.json`** (`port`) → **`hooks install`** creates **`.claude/settings.json`** + shim (**exit 0**).

**Invalid JSON (syntax):** **`hooks install`**, **`hooks cursor install`**, **`hooks codex install`** on **`{ bad`** (truncated) → **exit 1**, stderr **`refusing to overwrite … — it contains invalid JSON:`**; **config bytes unchanged**; **no** **`coach-cursor-hook.sh`** / **`codex-hook.sh`** on Cursor/Codex failure path (parse before shim).

**Valid JSON, wrong shape:** **`[1,2,3]`** in **`~/.claude/settings.json`** → **exit 1**, **`config file is not a JSON object`**.

**Permission / write failure:** As **`root`**, **`chmod a-w`** on **`settings.json`** does **not** block writes — **`hooks install` still succeeds** (expected Unix root behavior). To force a real failure: **`chattr +i ~/.claude/settings.json`** (valid **`{}`**) → **`hooks install`** → **exit 1**, **`Operation not permitted (os error 1)`**. **Finding:** **`claude-hook.sh`** may still be written under **`~/.coach/`** before the final config write fails — possible **orphan shim** if JSON merge/write fails after shim creation (not specific to `chattr`; same ordering for any late write error).

**One-liner — rebuild + inspect artifact on VPS:**

```bash
ssh root@46.225.111.102 'bash -lc "cd /root/coach && export PATH=/root/.cargo/bin:/usr/bin:/bin && cargo build --release -p coach && ./target/release/coach --version && file ./target/release/coach"'
```

**Minimal isolated smoke (copy-paste on VPS):**

```bash
COACH=/root/coach/target/release/coach
H=$(mktemp -d)
export HOME="$H"
mkdir -p "$HOME/.coach" && echo '{"port":7700}' > "$HOME/.coach/settings.json"
env -i HOME="$HOME" USER=test PATH=/usr/bin:/bin "$COACH" path install
env -i HOME="$HOME" USER=test PATH="$HOME/.local/bin:/usr/bin:/bin" "$COACH" hooks install
env -i HOME="$HOME" USER=test PATH=/usr/bin:/bin "$COACH" hooks cursor install
env -i HOME="$HOME" USER=test PATH=/usr/bin:/bin "$COACH" hooks codex install
rm -rf "$H"
```

### Historical: E2E after `RunEvent::Reopen` fix

Earlier pass: **`npm run build`**, **`cargo build --release -p coach`**, **`file`** → ELF **aarch64** PIE; version numbers drift with releases — use **`coach --version`** on the artifact under test.

### Historical: bug fixed — `RunEvent::Reopen` is macOS-only

`tauri::RunEvent::Reopen` exists only on macOS (Tauri 2). Unconditional match arm broke Linux builds (`E0599`). **Fix:** `#[cfg(target_os = "macos")]` on that arm; `_app_handle` on non-macOS. `src-tauri/src/lib.rs` ~146–154.

### Exact commands for repro

```bash
# Transfer (from macOS, repo root)
rsync -avz --exclude 'target/' --exclude 'node_modules/' --exclude '.claude/' \
  --exclude '.kodo/' --exclude 'dist/' --exclude '.git/' \
  ./ root@46.225.111.102:/root/coach/

# On VPS
ssh root@46.225.111.102
cd /root/coach
npm install
npm run build
# optional cold compile: cargo clean && cargo build --release -p coach
cargo build --release -p coach
./target/release/coach --version
cargo test --workspace   # optional full Rust suite on VPS
npm test
```

## Misc

- `npm audit` advisories are separate from functional tests.
