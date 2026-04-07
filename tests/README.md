# Coach test suite

Five layers, sorted by what each one proves and how cheap it is to run.
The further down you go, the slower and the more real-world coverage
you get.

```
                                                       runs in CI?    needs binary?    needs services?
1. unit tests          src-tauri/src/**/*.rs   #[cfg(test)] mod   yes               no              no
2. CLI integration     src-tauri/tests/cli_integration.rs           yes               yes             no
3. hook integration    src-tauri/tests/hook_integration.rs          yes               no              no  (2 ignored need real claude/cursor)
4. user-story smoke    tests/USER_STORIES.md                        no                yes             varies (real claude / cursor / Anthropic)
5. UI smoke            tests/test_ui_smoke.py                       macOS only        yes             needs WindowServer
```

If a regression makes you rewrite a test, write it at the **lowest
layer that can catch it**. A unit test for a state transition. A CLI
integration test for a flag-parsing bug. A hook integration test for a
hook contract change. A user story only when nothing else can prove the
property — the binary on a real OS, the daemon talking to a real agent,
the LLM with a real key.

---

## 1. Unit tests — `cargo test --lib`

~120 tests. `#[cfg(test)] mod` blocks scattered across the lib
(`state`, `settings`, `replay`, `pid_resolver`, `rules`, `scanner`,
`llm`, `commands`, `path_install`, ...). Pure logic — no filesystem
outside `tempfile`, no sockets, no subprocesses.

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

These run in <10 s. They catch state-machine bugs, snapshot
serialization regressions, hook-merge edge cases, rule-engine logic.

---

## 2. CLI integration — `cargo test --test cli_integration`

17 tests. Each one spawns the actual `coach` binary as a subprocess
with `HOME=$tmpdir` and asserts on exit code, stdout/stderr, and the
files it leaves behind. **The whole point is catching regressions in
`main.rs` dispatch wiring** — accidentally calling `coach_lib::run()`
before `dispatch()` would silently start Tauri on every CLI invocation,
and these tests would catch it because the Tauri startup banner would
appear in stderr.

```sh
cargo test --manifest-path src-tauri/Cargo.toml --test cli_integration
```

Notable tests:

- `version_subcommand_does_not_start_tauri` / `help_subcommand_does_not_start_tauri`
  — guard the dispatch wiring
- `cli_hooks_install_matches_install_hooks_at` — CLI install must
  produce a byte-identical `~/.claude/settings.json` to the helper
- `cli_serve_starts_headless_daemon_and_round_trips_config` — full
  end-to-end smoke for `coach serve`: spawn process, hit HTTP, set
  config via the CLI, verify it round-trips through both the HTTP
  layer and the on-disk file
- `cli_serve_exits_nonzero_on_port_collision` — regression for the
  A5 bug (serve used to panic in a worker and exit 0 on bind failure)
- `cli_serve_releases_port_on_kill` — kernel actually frees the port
  when the daemon dies

These are the tests that survive a refactor. Add to this layer when a
bug crosses the binary boundary (e.g. dispatch, env handling, CLI <->
file <-> HTTP routing, exit codes).

---

## 3. Hook integration — `cargo test --test hook_integration`

27 tests + 2 ignored. Spins up the Axum router on an OS-assigned port
in-process and hits it with `reqwest`. PID resolution is mocked via
`fake_resolver_from_sid` (hashes the session_id to a deterministic
non-zero u32) so tests can predict what session row appears.

```sh
cargo test --manifest-path src-tauri/Cargo.toml --test hook_integration
```

Run the two ignored tests (need `claude` / `cursor-agent` on PATH):

```sh
cargo test --manifest-path src-tauri/Cargo.toml --test hook_integration -- --ignored
```

Notable tests:

- `post_tool_use_creates_session` — basic happy path
- `multiple_sessions_tracked_independently` — two sessions, two
  fake PIDs, no cross-talk
- `clear_replaces_session_in_same_window` — `/clear` keeps the same
  PID and resets counters (the SESSION_TRACKING.md design property)
- `permission_request_auto_approves_in_away_mode` — Coach's
  away-mode contract with claude
- `stop_blocks_then_allows_on_cooldown` — 15 s STOP_COOLDOWN
- `cursor_after_shell_tracks_session` — Cursor's synthetic-PID path
- `all_hook_responses_conform_to_claude_code_schema` — schema
  conformance for everything claude expects back
- `observer_fires_in_llm_mode_and_records_failure` — LLM observer
  worker spawn + graceful failure on missing key

Add to this layer when changing the hook contract (request shape,
response shape, side effects on state) or the session-tracking model.

---

## 4. User-story smoke — `tests/USER_STORIES.md`

Manual / scripted end-to-end tests against real binaries on real
operating systems. The test plan groups them by what state they need
(install, daemon, real agent, LLM provider) and includes setup
templates and pass criteria. **Many stories already have automated
coverage at layers 1-3** — those are tagged inline with `Auto:` so you
can skip them on a VM run. The remaining stories are tagged
`VM only:` and are why this file exists.

The `VM only:` stories are the high-value ones to run on:

- a fresh macOS / Linux / Windows VM, before cutting a release
- after a bump of the `claude` CLI version
- after touching anything in `server.rs`, `pid_resolver.rs`,
  `scanner.rs`, or `lib.rs::run / lib.rs::serve`

Run the suggested smoke pipeline at the bottom of `USER_STORIES.md`
in order — it fails fast on anything fundamental (binary launch, hook
install, daemon binding) before getting to expensive stories
(real claude, real LLM).

Group tags inside the doc:
- **A** install & first launch
- **B** PATH shim
- **C** Claude Code hook installation
- **D** Cursor hook installation
- **E** config get/set
- **F** live state via the daemon
- **G** real Claude Code → Coach (highest value: G2 proves
  PID-based session tracking on the target OS)
- **H** Cursor Agent → Coach
- **I** sessions list & replay
- **J** LLM observer (Anthropic; requires `ANTHROPIC_API_KEY`)

---

## 5. UI smoke — `tests/test_ui_smoke.py`

macOS-only Python smoke that launches the bundled `coach` binary,
waits for the HTTP API to come up, then captures a screenshot of the
Coach window via `Quartz.CoreGraphics.CGWindowList` and asserts it
isn't blank white. Catches the "I broke the React build and the
window renders empty" regression that none of the lower layers can
see.

```sh
# Against an already-running Coach on port 7700:
uv run --with Pillow python tests/test_ui_smoke.py

# Or with launch + teardown:
uv run --with Pillow python tests/test_ui_smoke.py --launch
```

---

## Running the whole stack

```sh
# Layers 1-3 (~10 s total)
cargo test --manifest-path src-tauri/Cargo.toml

# Layers 1-3 including the ignored tests that need real CLI tools
# (claude / cursor-agent on PATH; live LLM tests need API keys)
cargo test --manifest-path src-tauri/Cargo.toml -- --include-ignored

# Layer 5 (macOS GUI only)
uv run --with Pillow python tests/test_ui_smoke.py --launch

# Layer 4 — pick groups from USER_STORIES.md and run the bash
# templates by hand. There is no umbrella runner script and that's
# deliberate — the value of layer 4 is exercising the binary on a
# real OS, not pretending we have CI.
```

Expected baseline at the time of writing: **~120 unit + 17 CLI + 27 hook
= ~165 passing, plus ~16 ignored**. The exact numbers drift as the
codebase grows; treat this as a floor, not an equality. If `cargo
test` ever drops below ~160 with no story explanation, something
regressed.

## Where to add a new test

| The bug is about... | Add a test at... |
|---|---|
| state machine, snapshot, hook merge, rule logic | Layer 1 (unit) |
| CLI flag parsing, exit codes, file output | Layer 2 (CLI integration) |
| HTTP request/response shape, session tracking | Layer 3 (hook integration) |
| something only a real binary on a real OS can show | Layer 4 (user story) |
| the GUI rendering, window state | Layer 5 (UI smoke) |

If you can write the test at two layers, write it at the lower one.
Lower-layer tests are faster, more deterministic, and survive
refactors better.
