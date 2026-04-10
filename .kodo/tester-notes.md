# Coach — tester notes

## Environment (verified 2026-04-10)

- **Binary:** Workspace outputs `coach` at **`/coach/target/release/coach`** (or `target/debug/coach` after `cargo test`). Not under `src-tauri/target/` alone — use workspace root `target/`.
- **Rust:** `cargo test --workspace` — **209** passed, **21** ignored (breakdown per crate: e.g. coach-core 162+15, hook_integration 29+2, cli 17, scenario_replay 1+4). Use `--workspace`; bare `cargo test` from repo root can be misleading if you only read the last `test result` line.
- **Node:** `npm test` — Vitest **380** tests, 36 files. `npm run build` — green (~1.5s Vite).
- **CLI:** `./target/release/coach --version` matches workspace `0.1.70`.

## UX gotchas (reconfirmed)

- **No per-subcommand help:** only top-level `coach`, `help`, `-h`, `--help` print usage.
- **`coach serve --help`** does **not** show help — it starts the headless daemon on default port 7700; stderr shows `[coach serve] listening...`. Kill the process if triggered during testing.

## Discovery docs

- **`.kodo/test-report.md`** — setup commands, CLI verbatim help, smoke table, baselines **209 / 21** + **380** Vitest; artifact path `target/release/coach` at repo root — **accurate**.
- **`.kodo/test-coverage.md`** — **589** = 209 + 380; Codex **HTTP** routes marked pending in hook_integration — confirmed no `codex` string in `coach-core/tests/hook_integration.rs` (routes exist in `server.rs`).

## Optional / not run here

- **`tauri dev` / full GUI:** long-lived; blocked without display server.
- **`cargo test -- --ignored`:** needs API keys / external processes.
- **`pycoach` feature:** separate feature flag; not default.

## Misc

- `npm audit` advisories are separate from functional tests.
