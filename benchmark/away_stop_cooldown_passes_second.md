# away_stop_cooldown_passes_second

**Source:** USER_STORIES G7
**Mode:** away (rules)

## What's happening

An agent in Away mode tries to end its turn. Coach blocks the Stop
with the priority list (same contract as G6). The agent receives
the block, does a bit more work, then immediately tries to stop
again. Coach must let that second Stop pass through — otherwise the
agent is stuck in an infinite "block / try / block / try" loop and
the conversation goes nowhere.

The hook sequence:

1. `UserPromptSubmit` — seed the session
2. `Stop` — **blocked** (first stop, no previous cooldown)
3. `PostToolUse(Read)` — agent does one more thing after being blocked
4. `Stop` — **passthrough** (within the 15-second cooldown window)

## Why we have this scenario

Without the cooldown, Away mode is a footgun. The coach would
block every Stop hook, and since Stop hooks fire whenever the
agent's turn ends, the agent would burn tokens forever trying to
finish a turn it's not allowed to finish. The cooldown is the
safety valve that makes "block and inject priorities" a one-shot
nudge rather than a denial-of-service.

This pairs with live-wire story G7 in
[tests/USER_STORIES.md](../tests/USER_STORIES.md) and with the
HTTP-level regression
`hook_integration::stop_blocks_then_allows_on_cooldown`. This
benchmark is the **human-readable version** of that property —
the file you point at to explain "why does Coach only block once
per 15 seconds?".

## Expected coach behavior

- **Stop #1** → `decision == "block"`, `reason` contains a priority
  substring (`"ship"` here, matching the scenario's priority list).
- **Post-tool-use** → passthrough (no intervention asserted; we
  don't care about this event's response, it just advances the
  narrative).
- **Stop #2** → response body is exactly `{}` (passthrough). The
  cooldown applies because `last_stop_blocked` was just set on the
  first Stop.

## Known limits

- Replay collapses wall-clock time to zero. Every event fires within
  microseconds of the previous one, so the second Stop is
  **guaranteed** to be inside the 15s cooldown window. This is
  exactly the property we want to check ("second stop inside the
  window passes"), but the complementary property — "second stop
  *after* the window blocks again" — can't be exercised here
  without time-injection machinery. That lives in the unit tests
  for the cooldown math, not in benchmarks.
- The cooldown constant itself (`STOP_COOLDOWN = 15s`) is not
  asserted here. If someone changes it to 0 or `Duration::MAX`,
  this benchmark still passes — the contract is "a second stop
  inside the window passes", not "the window is exactly 15s".
