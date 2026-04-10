# Coach — tester notes

## Environment (verified 2026-04-10)

- **Binary:** Workspace outputs `coach` at **`/coach/target/release/coach`** (or `target/debug/coach` after `cargo test`). Not under `src-tauri/target/` alone — use workspace root `target/`.
- **Rust:** `cargo test --workspace` — **209** passed, **21** ignored (breakdown per crate: e.g. coach-core 162+15, hook_integration 29+2, cli 17, scenario_replay 1+4). Use `--workspace`; bare `cargo test` from repo root can be misleading if you only read the last `test result` line.
- **Node:** `npm test` — Vitest **380** tests, 36 files. `npm run build` — green (~1.5s Vite).
- **CLI:** `./target/release/coach --version` matches workspace `0.1.70`.

## UX gotchas (reconfirmed)

- **No per-subcommand help:** only top-level `coach`, `help`, `-h`, `--help` print usage.
- **`coach serve --help`** does **not** show help — it starts the headless daemon on default port 7700; stderr shows `[coach serve] listening...`. Kill the process if triggered during testing.

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
- **`.kodo/test-coverage.md`** — **589** = 209 + 380; Codex **HTTP** routes marked pending in hook_integration — confirmed no `codex` string in `coach-core/tests/hook_integration.rs` (routes exist in `server.rs`).

## Optional / not run here

- **`tauri dev` / full GUI:** long-lived; blocked without display server.
- **`cargo test -- --ignored`:** needs API keys / external processes.
- **`pycoach` feature:** separate feature flag; not default.

## Misc

- `npm audit` advisories are separate from functional tests.
