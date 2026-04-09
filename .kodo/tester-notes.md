# Coach — tester notes

## Environment (2026-04-08)

- **Rust:** `cd src-tauri && cargo test` — **184** passed, **17** ignored (139 lib + 17 CLI + 28 hook + 0 pycoach without feature). `cargo test --all-features` — **186** passed (+2 pycoach_sidecar). `cargo clippy --all-targets --all-features -- -D warnings` must cover **integration tests** (`tests/*.rs`), not only lib.
- **Node:** `npm test` (Vitest **225**), `npm run build` — green.
- **CLI:** `src-tauri/target/debug/coach --version` after `cargo test`.

## Improve report (`~/.kodo/runs/20260408_135732/improve-report.md`)

- Prior bug: report said “30 passed / 2 ignored” for full `cargo test --all-features` — that was the **hook_integration** test *count* (28+2), not the workspace total. Verification section updated to **186 / 17** (all-features) and **184 / 17** (default).

## Optional / not run here

- **`tauri dev` / full GUI:** long-lived; `hook_integration` + `cli_serve_*` cover HTTP + `coach serve`.
- **`cargo test -- --ignored`:** needs API keys.
- **`pycoach_sidecar`:** needs `--all-features`; default run is 0 tests in that binary.

## Gotchas

- `npm audit` advisories are separate from functional tests.
