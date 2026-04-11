# Coach benchmark

Canonical coaching scenarios with **desired interventions**. Each
scenario is a small, self-contained sequence of hook events plus a
description of what Coach *should* do in response. The runner feeds
the events through the real `server::events::dispatch` path (same
code the live hook server runs) and checks the responses against
the scenario's expectations.

A benchmark scenario answers two questions at once:

1. **What should Coach do in this situation?** Written in English in
   the sibling `.md` file so a human can scan the directory and
   understand what behavior is being pinned down, without reading any
   Rust.
2. **Does Coach actually do that today?** Encoded as assertions in
   the `.json` file so `cargo test` can verify it mechanically.

Scenarios here are deliberately **tiny** — one to five events, one
session, one session_id. A benchmark that needs to boot the scanner,
launch real `claude`, or check PID resolution lives in
`tests/USER_STORIES.md`, not here. Benchmarks test Coach's
intervention logic, not the plumbing around it.

## Layout

```
benchmark/
├── README.md                                    ← this file
├── <scenario>.md                                ← human documentation
├── <scenario>.json                              ← input events + expectations
└── ...
```

Flat, two files per scenario, same stem. Skimming `ls benchmark/` is
meant to read like a table of contents: each filename is the
scenario's one-line summary.

## Scenarios

| Scenario | Source | One-line purpose |
|---|---|---|
| [away_stop_blocks_with_priorities](away_stop_blocks_with_priorities.md) | USER_STORIES G6 | In Away mode the first Stop must be blocked with the priority list injected as the block reason. |
| [away_stop_cooldown_passes_second](away_stop_cooldown_passes_second.md) | USER_STORIES G7 | After a Stop is blocked, a second Stop within the cooldown window must pass through — no stacked blocks. |
| [away_permission_auto_approved](away_permission_auto_approved.md) | USER_STORIES G5 | In Away mode a PermissionRequest must be auto-approved so the user can walk away without a modal. |

## Scenario file schema (`*.json`)

```jsonc
{
  "name": "away_stop_blocks_with_priorities",
  "source": "USER_STORIES G6",
  "description": "One-sentence machine-readable summary.",

  // How the isolated replay state is configured before any event fires.
  "mode": "away",                                  // "present" | "away" | "llm"
  "priorities": ["ship the test", "fix the bug"],  // optional; overrides defaults
  "session_id": "bench-g6",                        // the key used inside the isolated state

  // Events to dispatch in order. Each one is a synthetic hook POST
  // through the real server router. `session_id` is merged into every
  // body automatically — you don't repeat it per event.
  "events": [
    {
      "hook": "user-prompt-submit",
      "body": { "prompt": "what is 2+2" }
    },
    {
      "hook": "stop",
      "body": { "stop_reason": "end_turn" },

      // Assertions applied to the JSON response from this event's
      // dispatch. Every key is optional; any omitted key is simply
      // not checked. Mix as needed.
      "expect": {
        "decision": "block",                       // top-level {decision: "block"}
        "reason_contains": "ship the test",        // substring match on top-level {reason: ...}
        "context_contains": null,                  // substring match on hookSpecificOutput.additionalContext
        "permission": null,                        // "allow" → hookSpecificOutput.decision.behavior
        "passthrough": false                       // true → response body is exactly `{}`
      }
    }
  ]
}
```

### Supported hooks

The `hook` field maps directly to the Claude Code hook HTTP routes
registered by `server/claude.rs`. Supported values today:

- `session-start`
- `user-prompt-submit`
- `pre-tool-use`
- `post-tool-use`
- `permission-request`
- `stop`

Body shapes match what live Claude Code sends (see existing
`hook_integration.rs` tests for reference payloads).

### Supported `expect` keys

| Key | Type | Meaning |
|---|---|---|
| `decision` | `"block"` | Response has top-level `{decision: "block"}` — i.e. Coach blocked a Stop. |
| `reason_contains` | string | Substring match on top-level `reason` (paired with `decision: "block"`). |
| `context_contains` | string | Substring match on `hookSpecificOutput.additionalContext`. That's where rule messages and `[Coach]: …` interventions land on PostToolUse. |
| `permission` | `"allow"` | Shorthand for `hookSpecificOutput.decision.behavior == "allow"` — the Away-mode permission auto-approval shape. |
| `passthrough` | `true` \| `false` | `true` asserts the response body is exactly `{}`. Useful when the behavioral contract is "don't intervene *here*". |

If a scenario needs an assertion that isn't in this table, add the
key to the runner rather than hand-rolling it in JSON — assertions
should be declarative and reusable across scenarios.

## `.md` file contract

The sibling `.md` is documentation for humans. It has no machine
effect. Each one should cover, in order:

1. **What's happening** — the narrative. "User types X, walks away,
   agent does Y, coach should Z."
2. **Why we have this scenario** — the bug it would catch, the
   design property it pins down, the USER_STORIES entry it came from.
3. **Expected coach behavior** — restate the `expect` assertions
   from the JSON in English, so a reader who doesn't know JSON can
   still understand what's being tested.
4. **Known limits** — anything this scenario can't check. (E.g.
   cooldown timing: replay collapses wall-clock time to zero, so a
   scenario can exercise "second stop within cooldown window" but
   not "second stop *after* cooldown window" without extra machinery.)

Keep it under a page. If a scenario needs more than that to explain,
it's probably two scenarios pretending to be one.

## Running

The runner is `coach-core/tests/benchmark_suite.rs` — a data-driven
integration test that walks `benchmark/*.json`, boots a headless
coach server for each, dispatches the events, and asserts. Run with:

```
cargo test -p coach-core --test benchmark_suite
```

A failure points at the specific scenario, the specific event
index, and the specific `expect` key that didn't match. There's no
hidden state between scenarios — each one runs against a fresh
isolated `AppState`.

## Adding a scenario

1. Copy an existing pair as a starting point (`cp away_stop_blocks_with_priorities.{md,json} new_name.{md,json}`).
2. Edit the JSON: `name`, `description`, `events`, `expect`.
3. Edit the `.md`: tell the story, explain why it's here.
4. Add a row to the **Scenarios** table in this README.
5. `cargo test -p coach-core --test benchmark_suite` should pick it
   up automatically — no code changes to the runner.

## What belongs here, what doesn't

**Belongs here:** anything you'd describe as "when Coach sees X, it
should do Y". That's the whole job of the benchmark.

**Doesn't belong here:** scenarios that need a real agent process
(PID resolution, scanner bootstrap, shell integration, hook file
merge). Those stay in `tests/USER_STORIES.md` — they test the pipe,
not the brain.
