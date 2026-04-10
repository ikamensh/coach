# Worker Smart Notes

## Test Commands
- **TS**: `npm run test` (vitest run) — 380 tests in 36 files, ~1.3s
- **Rust**: `cargo test --workspace` — 219 tests (171 unit + 18 CLI integration + 29 hook + 1 scenario), ~13s + compile
- 21 ignored tests need live API keys or external CLI tools (15 coach_core, 2 hook_integration, 4 scenario_replay)

## Key Files
- TS tests: `src/components/{ActivityBar,SessionList,SettingsPane}.test.ts`
- Rust integration tests: `coach-core/tests/{cli.rs,hook_integration.rs,scenario_replay.rs}`
- Rust unit tests: inline in `coach-core/src/` modules
- CLI dispatch: `coach-core/src/cli.rs` — `dispatch_with_args()` is the testable entry point
- Reports: `.kodo/test-report.md`, `.kodo/test-coverage.md`

## Build
- `npm run install-app` for full GUI build+install
- `cargo build --release` for CLI-only
- Workspace: src-tauri + coach-core crates
- Version source of truth: package.json (currently 0.1.70, Cargo workspace 0.1.71)

## Linux Build
- VPS: root@46.225.111.102, Debian 12 ARM64, Rust 1.94.1, Node 22
- `RunEvent::Reopen` is macOS-only — gated with `#[cfg(target_os = "macos")]` in `src-tauri/src/lib.rs`
- All 219 Rust + 35 Vitest tests pass identically on Linux ARM64 (Stage 2 re-verified 2026-04-10)
- Transfer with rsync excluding target/node_modules/.claude/.git/.kodo
- Linux test compile ~3m14s (debug profile); test execution ~12.7s total

## Fixes Applied
- 2026-04-10: `coach serve --help` was starting daemon — added `wants_help()` guard + regression tests
- 2026-04-10: Hook install was silently overwriting invalid JSON config files — now fails safely with error; 4 regression tests added
- 2026-04-10: Hook install now atomic on invalid JSON — validates config before writing shim; 2 no-shim-left-behind tests + strengthened existing 2
- 2026-04-10: Frontend now threads `observer_capable_providers` from backend snapshot through TS types → store → SettingsPane UI; shows warning for non-observer providers in LLM mode; 4 regression tests
- 2026-04-10: `path uninstall` and `path status` now accept `--dir` to match `path install --dir`; 1 integration roundtrip test added
- 2026-04-10: `observer_does_not_fire_in_rules_mode` — **incorrect test, not a product bug**. Test never set `coach_mode = Rules` (default is `Llm`). Fixed by setting `EngineMode::Rules` on the test server state before the hook call. Verified on both macOS and Debian ARM64 VPS.

## Authoritative Test Counts (2026-04-10)
- **macOS + Debian ARM64 (identical):** 219 passed, 0 failed, 21 ignored
  - coach-core unit: 171 passed, 15 ignored
  - cli_integration: 18 passed
  - hook_integration: 29 passed, 2 ignored
  - scenario_replay: 1 passed, 4 ignored
- **Vitest:** 35 tests, 3 files
