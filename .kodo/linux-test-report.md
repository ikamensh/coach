# Coach — Linux test report (stages 1–5)

**Date:** 2026-04-10  
**Environment:** Debian **12** **ARM64** (aarch64), kernel **6.1.0-44-arm64**, host **`root@46.225.111.102`** (openclaw-1). Rust **1.94.1**, Node **22.x**. Workspace binary under test: **`/root/coach/target/release/coach`** (ELF PIE, dynamic linker `/lib/ld-linux-aarch64.so.1`). Most scenarios used isolated **`mktemp -d` `HOME`** and **`env -i HOME=… USER=test PATH=…`** so user hook/config trees do not touch the real admin home.

---

## Verdict

**Linux validation for stages 1–5 is complete and passing.** Automated parity matches macOS on Rust + Vitest counts. Manual E2E on the VPS covered CLI, PATH/hooks file workflows, daemon HTTP + hooks, load/stress, and (where applicable) frontend build/tests. The **orphan hook-shim** issue identified during earlier Linux work is **fixed in code and verified** by unit tests plus an immutable-config E2E on Debian (see below).

---

## Test counts (authoritative baseline)

| Layer | Result |
|-------|--------|
| **`cargo test --workspace`** | **221** passed, **0** failed, **21** ignored |
| **`npm test`** (Vitest) | **35** tests, **3** files — all pass |
| **Combined** | **256** passing automated tests (**221** Rust + **35** Vitest), **21** Rust ignored (API keys / external processes) |

**Per-crate Rust (typical breakdown on Linux):** `coach-core` unit **173** + **15** ignored; `hook_integration` **29** + **2** ignored; `cli_integration` **18**; `scenario_replay` **1** + **4** ignored; others as emitted by `cargo test --workspace`.

---

## Stage 1 — Discovery & Linux CLI / system integration

- **Scope:** Map Linux file/PATH behavior; shallow + deep CLI passes with isolated `HOME`.
- **Build:** `cargo build --release -p coach` → `target/release/coach` (e.g. **0.1.78** during the main pass).
- **Covered:** `--version`, `--help`, `serve --help` (no bind), `config get` / `config set`, `path install|status|uninstall`, Claude/Cursor hook install/status/uninstall with “dirty” valid JSON, **`hooks codex`** lifecycle, seed-only `HOME` flows.
- **Stage 1b (deep):** `serve --port`, `status` / `status --json`, `config set` via **HTTP** when daemon up vs **file** when down, **`curl`** hook smoke, `mode away|present`, second `serve` → **EADDRINUSE** (**os error 98** on Linux), Codex install/uninstall roundtrip.
- **Findings (cross-platform, documented in `.kodo/tester-notes.md`):** e.g. `hooks install --help` does not show help (runs install); minor labeling nits; `status --json` still plain text error when daemon down.

---

## Stage 2 — `cargo test --workspace` + `npm test` on Linux

- **`cargo test --workspace`:** **221** passed, **21** ignored — **same outcome as macOS** after fixes.
- **Notable fix:** `hook_integration::observer_does_not_fire_in_rules_mode` must set **`coach_mode = EngineMode::Rules`** in-test (`Settings::default()` is **`Llm`**; Linux env with API keys could otherwise make the observer fire and flake).
- **`pid_resolver::tests::resolves_real_connection_to_child_pid`:** passes on Linux (**`/proc/net/tcp`** / netstat2 path).
- **`npm test`:** **35** tests — pass (Vitest excludes `**/.claude/**` in `vite.config.ts`).

---

## Stage 3 — Release binary: PATH shim + external hooks (Claude / Cursor / Codex)

- **E2E:** `path install`, idempotent second run, `path uninstall` second call non-zero, dirty JSON idempotency (**sha256** stable after 2nd `hooks install` / `hooks cursor install` / `hooks codex install`), uninstall preserves user keys and non-Coach hooks, syntax-invalid JSON → **exit 1**, file unchanged, **no shim** (validation before write).
- **Orphan shim (historical issue → fixed and verified):**
  - **Previously:** `install_nested_hooks` / `install_cursor_hooks_at` could write the shell shim **before** the hook JSON was successfully merged/written. If the config write then failed (e.g. immutable file), **`~/.coach/claude-hook.sh`** (and Cursor counterpart) could remain **without** matching hook entries.
  - **Fix:** Write **config first**, **shim last** in `coach-core/src/settings/hooks.rs`.
  - **Rust regression (+2 tests, `#[cfg(unix)]`, skipped on root):** `install_nested_no_shim_when_config_not_writable`, `install_cursor_no_shim_when_hooks_json_not_writable`.
  - **Linux E2E (post-fix binary, e.g. `coach` 0.1.79):** isolated `HOME`, valid `{}` in `~/.claude/settings.json`, **`chattr +i`** on that file → **`coach hooks install`** exits **1**, **`~/.coach/claude-hook.sh` absent** — **no orphan**. Repro snippet in **`.kodo/tester-notes.md`**.

---

## Stage 4 — Daemon HTTP: hooks, PID resolution, load, shutdown

- **`coach serve`:** Claude **`/hook/…`**, Cursor **`/cursor/hook/…`**, Codex **`/codex/hook/…`**, **`GET /api/state`**, **`coach status`**; clean **SIGTERM**/Ctrl-C.
- **PID:** In-flight **`curl`** to Claude hooks: **`ss -tnp`** peer PID and ephemeral port match daemon logs (`resolved sid … → pid …`). Confirms Linux TCP peer resolution path on aarch64.
- **Stress:** Hundreds of concurrent hook POSTs (**200** for completed work); **`/api/state`** polls stable; shutdown under load frees listen port (racing requests **000**/reset as expected).
- **Model note:** Sessions keyed by **resolved OS PID**; short synthetic `session_id` strings can **collide** via `fake_pid_for_sid` — expected, not a concurrency bug (see `.kodo/tester-notes.md`).

---

## Stage 5 — Frontend (Vitest + build)

- **`npm run build`:** pass (`tsc -b` + Vite).
- **`npm test`:** **35** tests, **3** files — pass; scope is helpers + **`SettingsPane`** provider/observer-capability consistency tests (no full Tauri/webview).
- **`observer_capable_providers`:** mirrored in TS snapshot/store; HTTP **`GET /api/state`** includes field for parity with IPC.

---

## Build fix (Linux compile)

- **`tauri::RunEvent::Reopen`** is **macOS-only** (Tauri 2). Unconditional match caused **`E0599`** on Linux. **Fix:** `#[cfg(target_os = "macos")]` on that arm in `src-tauri/src/lib.rs`.

---

## Gaps / not claimed here

- Full **`tauri dev`** GUI on Linux (display/server not in automated pass).
- **`cargo test -- --ignored`** (needs keys/processes).
- **`pycoach` feature** tests unless explicitly enabled with tooling.

---

## References

- Detailed matrices and repro commands: **`.kodo/test-coverage.md`**, **`.kodo/tester-notes.md`**
- macOS discovery snapshot (older Vitest counts possible): **`.kodo/test-report.md`**
