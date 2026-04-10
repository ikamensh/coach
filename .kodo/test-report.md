# Coach — test / discovery report

**Date:** 2026-04-10  
**Host:** macOS (darwin 25.4.0), workspace `/Users/ikamen/ai-workspace/ilya/coach`

## Setup commands (executed)

| Step | Command | Result |
|------|---------|--------|
| npm deps | `cd .../coach && npm install` | Exit 0. `up to date, audited 112 packages`. npm reported 1 high severity vulnerability (`npm audit` for detail). |
| Release binary | `cd .../coach/src-tauri && cargo build --release` | Exit 0. Finished release in ~46s. |
| Rust tests | `cd .../coach && cargo test` | Exit 0. See baseline counts below. |
| Frontend unit tests | `npm test` | Exit 0. Vitest 380 tests, 36 files. |
| Frontend production build | `npm run build` | Exit 0. Vite build ~1.3s. |

**Artifact path:** This repo is a Cargo workspace; the `coach` executable is at **`coach/target/release/coach`** (not under `src-tauri/target/`).

## CLI — behavior notes

- **Usage text** is only printed for: no subcommand + `help` / `-h` / `--help`, or `version` / `-V` / `--version`. There are **no** per-subcommand help handlers (`coach status --help` is parsed as status with a bad flag path, not as help).
- **`coach serve --help` starts the headless daemon** — `serve` ignores unknown tokens for port parsing; `--help` does not print help. Stop with Ctrl+C or kill the process. (Accidentally started once during discovery; port 7700 cleared afterward.)

## CLI — `coach --help` (verbatim)

```
coach — Claude Code companion (GUI + CLI)

USAGE:
    coach                                  launch the GUI
    coach <command> [args]                 run a CLI subcommand

COMMANDS:
    serve [--port N]                       run the daemon headless (no GUI / no tray)
    status [--json]                        show live state (requires running Coach)
    mode <away|present> [--pid N]          set away/present mode (requires running Coach)

    hooks status                           show Claude Code hook installation status
    hooks install                          install Coach hooks into ~/.claude/settings.json
    hooks uninstall                        remove Coach hooks

    hooks codex status                     show Codex CLI hooks (~/.codex/hooks.json)
    hooks codex install                    install Coach hooks into Codex hooks.json
    hooks codex uninstall                  remove Coach-managed Codex hook entries

    hooks cursor status                    show Cursor Agent hooks (~/.cursor/hooks.json)
    hooks cursor install                   add curl forwarders to Cursor hooks.json
    hooks cursor uninstall                 remove Coach-managed Cursor hook entries

    path install [--dir DIR]               install a `coach` shim on PATH
    path uninstall                         remove the PATH shim
    path status                            show PATH shim status

    config get [<key>]                     read settings
    config set priorities <a,b,c>          replace priorities list
    config set model <provider> <model>    set the LLM model
    config set api-token <provider> <tok>  store an API token
    config set coach-mode <rules|llm>      switch the coach engine
    config set rule <id> <on|off>          enable/disable a rule

    sessions list [--limit N] [--json]     list saved Claude Code sessions
    replay <session-id> [--mode away|present|llm] [--json]

    help, --help, -h                       this message
    version, --version, -V                 print version
```

## CLI — `coach --version`

```
coach 0.1.70
```

## Smoke checks (non-help)

| Command | Result |
|---------|--------|
| `coach status` | Exit 1: Coach not running on port 7700 (expected when daemon/GUI down). |
| `coach hooks status` | Exit 0: listed Claude `settings.json` hook targets. |
| `coach path status` | Exit 0: reported shim under `~/.local/bin/coach`. |
| `coach config get` | Exit 0: JSON settings to stdout. |
| `coach sessions list --limit 1` | Exit 0: listed saved sessions. |

## Test baselines (this run)

| Suite | Passed | Ignored / skipped |
|-------|--------|---------------------|
| `cargo test` (workspace) | 209 | 21 ignored |
| `npm test` (Vitest) | 380 | — |

**Rust breakdown (approx.):** `coach-core` unit 162 + `cli_integration` 17 + `hook_integration` 29 + `scenario_replay` 1; ignored includes live LLM / real Claude-Cursor tests.

## Feature inventory (for coverage tracking)

Top-level CLI verbs: `serve`, `status`, `mode`, `hooks` (with `codex` and `cursor` sub-trees), `path`, `config`, `sessions`, `replay`; meta: `help`, `version`; bare `coach` → GUI.

Integration surface (from codebase / prior notes): Claude hooks, Cursor hooks, Codex hooks + HTTP `/codex/hook/...`; HTTP hooks for Claude under `/hook/...`; REST `/api/*` when daemon up.

## Issues

None blocking this discovery. **Quirk:** `coach serve --help` starts the server — document and avoid assuming POSIX `--help` on subcommands.
