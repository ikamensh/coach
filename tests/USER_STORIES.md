# Coach — VM-testable user stories

Stories that can be exercised end-to-end from a shell on a clean VM,
with an assertion an automation script can check. Grouped by what kind
of state they need. Each story has:

- **Setup** — what the VM needs before the story can run
- **Steps** — exact commands
- **Pass** — observable signal (exit code, file content, JSON field)
- **Coverage** — `Auto via <test>` if a Rust test already proves this
  property, or `VM only` if it can only be exercised against a real
  binary on a real OS

See `tests/README.md` for the full test stack and where each layer
fits. **The high-value stories on a VM run are the ones tagged
`VM only`** — the `Auto via …` ones already pass in `cargo test` and
add little signal beyond confirming the build.

The point is to catch the regressions a unit test can't see: shipped
binary actually launching, hooks actually merging into a real
`~/.claude/settings.json`, two real `claude` processes resolving to two
distinct PIDs, etc.

Platforms in scope: macOS (arm64 + x64), Linux (deb + AppImage), Windows
x64. Mark per-story which platforms it must run on.

---

## A. Install & first launch (no daemon)

### A1. `coach version` works on a fresh box
- **Setup**: just-installed binary, no `~/.coach`, no `~/.claude`.
- **Steps**: `coach version`
- **Pass**: exit 0, stdout starts with `coach `, stderr does **not**
  contain `starting up` (proves CLI dispatch short-circuited Tauri).
- **Platforms**: all.
- **Coverage**: Auto via `cli_integration::version_subcommand_does_not_start_tauri`

### A2. `coach help` lists every documented subcommand
- **Steps**: `coach help`
- **Pass**: exit 0; stdout contains `status`, `mode`, `hooks`, `path`,
  `config`, `sessions`, `replay`.
- **Coverage**: Auto via `cli_integration::help_subcommand_does_not_start_tauri`

### A3. Unknown subcommand exits 2 with a usage hint
- **Steps**: `coach nope`
- **Pass**: exit 2, stderr contains `unknown command`.
- **Coverage**: Auto via `cli_integration::unknown_command_exits_two`

### A4. Headless `coach serve` binds the configured port
- **Setup**: clean `$HOME` with `~/.coach/settings.json` containing
  `{"port":7711}`.
- **Steps**: `HOME=$tmp coach serve --port 7711 &`, wait for port,
  then `HOME=$tmp coach status --json`.
- **Pass**: `coach status` exit 0, JSON has `port == 7711` and a
  (possibly empty) `sessions` array.
- **Notes**: Headless serve does not need a display server, tray, or
  webview, so it works on every VM the binary builds for. The GUI
  launcher (`coach` with no subcommand) hits the single-instance
  plugin and is unsuitable for automated testing — see story F.
- **Coverage**: Auto via `cli_integration::cli_serve_starts_headless_daemon_and_round_trips_config`

### A5. Second `coach serve` on the same port fails fast
- **Setup**: instance from A4 still running on 7711.
- **Steps**: spawn another `coach serve --port 7711` (same or
  different `$HOME`).
- **Pass**: second process exits non-zero with a bind error;
  the first daemon is still alive and answering `/version`.
- **Coverage**: Auto via `cli_integration::cli_serve_exits_nonzero_on_port_collision`

---

## B. PATH shim

### B1. `coach path install --dir <D>` puts an executable shim in D
- **Steps**: `coach path install --dir $HOME/bin`
- **Pass**: `$HOME/bin/coach` exists, is executable, and resolves
  (via symlink or wrapper) to the installed binary.
- **Platforms**: Unix (symlink); Windows uses a `.cmd` wrapper — verify
  it runs.
- **Coverage**: Auto via `cli_integration::cli_path_install_creates_shim_in_chosen_dir`

### B2. After `path install`, a brand-new shell can find `coach`
- **Steps**: in a new shell with `$HOME/bin` on PATH, run `coach version`.
- **Pass**: same as A1.
- **Coverage**: VM only — the auto test verifies the symlink, not
  PATH discovery from a fresh shell.

### B3. `coach path uninstall` removes the shim cleanly
- **Pass**: `coach path status` shows `installed: false`, the shim file
  is gone, no other files in the install dir were touched.
- **Coverage**: VM only.

---

## C. Claude Code hook installation

### C1. Install into a fresh `~/.claude`
- **Setup**: no `~/.claude/settings.json`.
- **Steps**: `coach hooks install`
- **Pass**: file exists, contains all 4 hook URLs:
  `permission-request`, `stop`, `post-tool-use`, `user-prompt-submit`.
  (SessionStart used to be in this list — see the comment on
  `expected_hook_urls` for why it was removed.) `coach hooks status`
  reports `all installed: true`.
- **Coverage**: Auto via `cli_integration::cli_hooks_install_matches_install_hooks_at`

### C2. Install merges with existing user hooks (does not clobber)
- **Setup**: write a `~/.claude/settings.json` containing one user-defined
  command hook (e.g. PreToolUse → `echo`).
- **Steps**: `coach hooks install`
- **Pass**: the user hook is still present byte-for-byte; coach hooks
  added alongside. Use `jq` to assert both keys exist.
- **Coverage**: Auto via `settings::tests::install_hooks_preserves_existing_settings`

### C3. Install is idempotent
- **Steps**: `coach hooks install` twice.
- **Pass**: second run exit 0; file diff between runs is empty.
- **Coverage**: Auto via `settings::tests::install_hooks_is_idempotent`

### C4. Uninstall removes only coach entries
- **Setup**: from C2 (user hook + coach hooks).
- **Steps**: `coach hooks uninstall`
- **Pass**: user hook still present; no coach URLs remain anywhere in
  the file. `coach hooks status` shows zero ✓ marks.
- **Coverage**: Auto via `cli_integration::cli_hooks_install_then_uninstall_round_trip`
  + `settings::tests::uninstall_reverses_install`

### C5. Custom port from settings is honoured
- **Setup**: pre-write `~/.coach/settings.json` with `{"port": 7711}`.
- **Steps**: `coach hooks install`
- **Pass**: every URL in the installed file says `localhost:7711`.
- **Coverage**: VM only — could be lifted to a settings.rs unit test.

---

## D. Cursor hook installation (Unix)

### D1. Install writes a shebanged, executable shim that calls curl
- **Steps**: `coach hooks cursor install`
- **Pass**: `~/.cursor/coach-cursor-hook.sh` exists, mode includes any
  exec bit, content starts with `#!/bin/sh` and contains `curl`.
  `~/.cursor/hooks.json` references the shim by absolute path.
  *(Regression for the Cursor-curl-block memory: we install a shim
  because Cursor silently rejects raw `curl` commands.)*
- **Coverage**: Auto via `cli_integration::cli_hooks_cursor_install_matches_install_cursor_hooks_at`

### D2. Cursor uninstall removes coach entries only
- Same merge property as C4 against `~/.cursor/hooks.json`.
- **Coverage**: VM only.

---

## E. Config get/set

These stories run in **file mode** (no daemon) and **HTTP mode** (daemon
running). Both must produce the same observable result.

### E1. Set/get round-trip — file mode
- **Setup**: no daemon; `~/.coach/settings.json` with `port: 1` to
  disable the running-server probe.
- **Steps**: `coach config set priorities A,B,C` then
  `coach config get priorities`
- **Pass**: stdout lists `1. A`, `2. B`, `3. C`; settings.json
  `priorities == ["A","B","C"]`.
- **Coverage**: Auto via `cli_integration::cli_config_set_priorities_persists_to_file`
  + `cli_config_get_reads_what_set_wrote`

### E2. Set/get round-trip — HTTP mode
- **Setup**: daemon running on the default port.
- **Steps**: `coach config set priorities X,Y` then `coach status --json`
- **Pass**: `coach status --json | jq .priorities` equals `["X","Y"]`,
  AND `~/.coach/settings.json` was rewritten with the same value.
  *(Both code paths must agree — this is the regression test for the
  CLI/HTTP split.)*
- **Coverage**: Auto via `cli_integration::cli_serve_starts_headless_daemon_and_round_trips_config`

### E3. Setting one key preserves unrelated keys
- **Steps**: set priorities, then set model, then read the file.
- **Pass**: priorities are still the values from step 1; model has
  the new values; nothing else lost.
- **Coverage**: Auto via `cli_integration::cli_config_set_preserves_unrelated_fields`

### E4. `config set rule` merges into the existing rule list
- **Setup**: defaults include the `outdated_models` rule.
- **Steps**: `coach config set rule custom_check on`
- **Pass**: `outdated_models` still present; `custom_check` added.
- **Coverage**: Auto via `cli_integration::cli_config_set_rule_merges_into_existing_list`

### E5. `coach status` errors cleanly when no daemon is running
- **Setup**: no daemon.
- **Steps**: `coach status`
- **Pass**: exit 1, stderr says Coach is not running on port N.
  No file writes, no hangs.
- **Coverage**: VM only — should probably be lifted to a small
  cli_integration test.

---

## F. Live state via the daemon

These all need a running daemon. **Use `coach serve --port <free port>`,
not `coach &`.** The GUI launcher hits `tauri-plugin-single-instance`,
whose macOS implementation uses a global Unix socket keyed only on the
bundle identifier — so a second GUI coach on the same user account
silently bounces to the first one and exits 0. The headless `serve`
subcommand bypasses Tauri entirely.

Setup template for every story below:
```sh
tmp=$(mktemp -d)
mkdir -p "$tmp/.coach"
echo '{"port":7711}' > "$tmp/.coach/settings.json"
HOME=$tmp /path/to/coach serve --port 7711 &
COACH_PID=$!
trap 'kill $COACH_PID 2>/dev/null' EXIT
# Wait for the port (10s ceiling).
for i in {1..50}; do nc -z 127.0.0.1 7711 && break; sleep 0.2; done
```

### F1. `coach mode away` flips every session
- **Setup**: daemon, two simulated sessions (or two real `claude`
  processes — see G).
- **Steps**: `coach mode away`
- **Pass**: `coach status --json | jq '.sessions[].mode'` returns all
  `"away"`.
- **Coverage**: Auto via `hook_integration::api_set_all_sessions_mode_flips_every_session`
  (HTTP-level; the VM run additionally exercises the CLI → daemon path).

### F2. `coach mode away --pid N` flips one session only
- **Steps**: `coach mode away --pid <pid_of_session_1>`
- **Pass**: that session is `away`; others unchanged.
- **Coverage**: Auto via `hook_integration::api_set_session_mode_targets_one_pid`
  + `api_set_session_mode_404_for_unknown_pid`

### F3. Daemon survives rapid HTTP requests
- **Steps**: 100 parallel `coach status --json` calls (xargs/parallel).
- **Pass**: all exit 0, no panics in daemon stderr, daemon still
  responsive after.
- **Coverage**: VM only — soak property against a real binary.

### F4. Daemon restart re-bootstraps sessions from scanner
- **Setup**: a real `claude` process running in some cwd.
- **Steps**: stop daemon, restart daemon, run `coach status`.
- **Pass**: the running `claude` PID appears in the new daemon's
  session list (the scanner picked it up from `~/.claude/sessions/*.json`
  even before any hook fires).
- **Coverage**: VM only — needs a real claude process for the scanner
  to discover.

---

## G. Real Claude Code → Coach hook flow

Needs `claude` CLI on the VM **and** `coach hooks install` already run.
These are the highest-value stories — they prove the wire is live.
**Every story in this group is `VM only`** — `cargo test` cannot
spawn a real claude process. The corresponding hook contract / state
machine properties are covered at the unit and HTTP level by
`hook_integration.rs`, but only a real claude run proves Claude
Code's debug log doesn't ship a regression that breaks the wire
(see G3 / G5 history).

### G1. One `claude` window → one Coach session row
- **Setup**: daemon up, hooks installed, fresh tempdir as cwd.
- **Steps**: in tempdir, run `claude -p "ls"` (one-shot mode), wait for
  exit, then `coach status --json`.
- **Pass**: exactly one session in the JSON, with `cwd` matching the
  tempdir, `event_count >= 1`.
- **Coverage**: VM only.

### G2. Two `claude` windows in the **same cwd** → two distinct sessions
- **Setup**: as G1.
- **Steps**: launch `claude` in two terminals in the same cwd,
  trigger one tool call in each, then `coach status --json`.
- **Pass**: two sessions, two distinct PIDs, both `cwd`s match.
  *(This is the property the SESSION_TRACKING.md design is built
  around — kernel-level peer-port → PID resolution. A regression here
  collapses the two windows into one row.)*
- **Coverage**: VM only — the auto test
  `hook_integration::multiple_sessions_tracked_independently` uses
  the fake resolver, so it can't catch a `netstat2` / `lsof_resolver`
  bug on a specific OS. Run G2 on every supported platform.

### G3. `/clear` inside one window → still one session, counters reset
- **Setup**: G1 with `event_count > 0` already.
- **Steps**: in the same `claude` window, run `/clear`, then trigger
  another tool call, then `coach status --json`.
- **Pass**: still exactly one session for that PID; `event_count == 1`
  again; `started_at` is recent.
- **Known issues**: claude 2.1.92 silently drops HTTP `SessionStart`
  hooks (`coach hooks install` no longer registers it), so `/clear`
  is detected lazily on the next tool call via the session_id
  mismatch path in `apply_hook_event`. This story is also hard to
  exercise in `-p` mode because `/clear` is interpreted as a
  skill name, not a slash command. Run interactively or skip.
  The state-machine property is covered by
  `hook_integration::clear_replaces_session_in_same_window` at the
  HTTP level.

### G4. Window exit → session GC'd
- **Steps**: kill the `claude` process; wait one scanner tick (~5s);
  `coach status --json`.
- **Pass**: that PID is gone from the sessions array.
- **Coverage**: VM only. Note: claude deletes its own
  `~/.claude/sessions/<pid>.json` on exit, so the scanner GCs the
  session almost immediately (faster than the 5 s tick). Query
  `status` *while* claude is alive to assert presence.

### G5. Away mode auto-approves a permission request
- **Setup**: G1 setup; `coach mode away` first.
- **Steps**: run a `claude` session that triggers a tool requiring
  permission (e.g. write to a file outside the project).
- **Pass**: no interactive prompt is shown; the action proceeds; the
  session's snapshot has the relevant tool in `tool_counts` and an
  activity entry tagged `auto-approved`.
- **Known issues**: claude 2.1.92 `-p` (print) mode does not POST
  PermissionRequest over HTTP — denied tools just fail inside claude
  with no hook fired. This story is unreachable from non-interactive
  claude. The hook contract itself is covered by
  `hook_integration::permission_request_auto_approves_in_away_mode`.
  Mark as **interactive-claude only** until the upstream regression
  is fixed.

### G6. Away mode blocks Stop and injects priorities
- **Setup**: as G5; `coach config set priorities "ship the test,fix the bug"`.
- **Steps**: run `claude -p "what's 2+2"` so the agent stops quickly.
- **Pass**: agent receives an injected message containing the priority
  list; `coach status --json` shows `stop_blocked_count >= 1` for that
  PID.
- **Coverage**: HTTP contract via `hook_integration::stop_blocks_then_allows_on_cooldown`;
  end-to-end live wire is `VM only`.

### G7. Cooldown: only one block per N seconds
- **Steps**: trigger Stop twice within the cooldown window.
- **Pass**: first Stop is blocked, second Stop passes through;
  `stop_blocked_count` only incremented once.
- **Coverage**: HTTP contract via `hook_integration::stop_blocks_then_allows_on_cooldown`
  (15 s `STOP_COOLDOWN`); end-to-end live wire is `VM only`.

---

## H. Cursor Agent → Coach hook flow

Needs `cursor-agent` CLI on the VM and `coach hooks cursor install`.

### H1. One Cursor session shows up in Coach
- **Steps**: run a `cursor-agent` command that triggers an after-shell
  hook; then `coach status --json`.
- **Pass**: a session row with the synthetic Cursor PID and a
  non-zero `event_count`.
- **Coverage**: HTTP contract via `hook_integration::cursor_after_shell_tracks_session`;
  live wire is `VM only`.
- **Known issues**: on macOS, `cursor-agent` invoked under
  `HOME=$tmpdir` triggers a system Keychain dialog (it tries to
  access the `cursor-user` keychain item from the user's *real* login
  keychain, which isn't reachable under a fake `$HOME`). Run H1
  against the user's *real* `$HOME` instead — and accept that this
  touches `~/.cursor`.

---

## I. Sessions list & replay (file-based, no daemon needed)

### I1. `sessions list` on a box with no `~/.claude/projects`
- **Setup**: clean `$HOME`.
- **Steps**: `coach sessions list --limit 5`
- **Pass**: exit 0, stdout contains `0 saved session`.
- **Coverage**: Auto via `cli_integration::cli_sessions_list_handles_missing_projects_dir`

### I2. `sessions list` enumerates real saved sessions
- **Setup**: copy a known fixture `.jsonl` into
  `~/.claude/projects/<encoded-cwd>/<sid>.jsonl`.
- **Steps**: `coach sessions list --json`
- **Pass**: array length >= 1, the fixture's id appears.
- **Coverage**: VM only — needs a real claude transcript fixture.
  Could be lifted to a unit test with a checked-in fixture.

### I3. `replay <id>` against a real session prints a summary
- **Setup**: as I2.
- **Steps**: `coach replay <sid> --json`
- **Pass**: JSON contains `message_count > 0`, `event_count >= 0`,
  `topic` (possibly empty string), and `first_intervention_index` is
  either null or a non-negative integer.
- **Coverage**: VM only.

### I4. `replay` of an unknown id errors cleanly
- **Steps**: `coach replay no-such-session-xyz`
- **Pass**: exit 1, stderr contains `not found`. No stack trace.
- **Coverage**: Auto via `cli_integration::cli_replay_unknown_session_errors_cleanly`

---

## J. LLM observer (Anthropic only — OpenAI key is dead per TODO.md)

Gated by `ANTHROPIC_API_KEY` being present on the VM.

### J1. Coach in `llm` mode emits an observer note for a real session
- **Setup**: `coach config set api-token anthropic $ANTHROPIC_API_KEY`,
  `coach config set coach-mode llm`,
  `coach config set model anthropic claude-haiku-4-5-20251001`,
  daemon running, hooks installed.
- **Steps**: run a `claude` session with a few tool calls, then
  `coach status --json`.
- **Pass**: that session's snapshot has a non-empty
  `coach_last_assessment`.
- **Cost guard**: cap the test session at e.g. 5 tool calls.
- **Coverage**: Observer worker spawn + failure path covered by
  `hook_integration::observer_fires_in_llm_mode_and_records_failure`;
  end-to-end with a real Anthropic key is `VM only`.

### J2. Stop evaluation in away mode uses the LLM and produces a decision
- **Steps**: as J1, with `coach mode away`, then trigger Stop.
- **Pass**: stop_blocked_count increments **and** the message the
  agent sees was generated by the model (assert it differs from the
  fixed `away_message` fallback). Or: assert `Observer/noted` events
  appear in the activity log.
- **Coverage**: Fallback path covered by
  `hook_integration::stop_in_llm_mode_falls_back_to_fixed_when_no_key`;
  end-to-end with a real key is `VM only`.

---

## Suggested smoke pipeline for a VM run

A script that hits the most-likely-to-break stories first:

```
A1  → A4  → C1  → C3  → C4  → E1  → E5
              ↓
            (start daemon)
              ↓
            E2  → F1  → F4
              ↓
            (install claude CLI; G1, G2, G3, G5, G6)
              ↓
            (anthropic key present?) → J1
```

If A1 or A4 fails, stop — the binary is broken.
If C1 fails, hook integration is broken; G* will all fail downstream.
G2 and G3 are the **critical** stories — they prove the
PID-resolution design from SESSION_TRACKING.md actually works on this
OS. They should run on every supported platform.
