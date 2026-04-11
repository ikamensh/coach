# away_stop_blocks_with_priorities

**Source:** USER_STORIES G6
**Mode:** away (rules)

## What's happening

The user is away. They've told Coach their priorities are *"ship the
test"* and *"fix the bug"*. The agent (running in Claude Code) gets
asked a one-shot question — `what is 2+2` — and immediately reaches
its turn's end. No tool calls, no investigation, just a quick
answer.

From Coach's vantage point, the hook sequence is:

1. `UserPromptSubmit` — the agent sees the user message
2. `Stop` — the agent is about to end its turn

Because the session is in **Away mode**, Stop is the moment where
Coach has to intervene. Letting the agent end its turn would mean
the user comes back to a coaching session that did nothing toward
their priorities.

## Why we have this scenario

This is the canonical **away-mode interrupt**. If it stops working,
the whole "coach watches while you're gone" feature stops working —
the agent happily ends the turn and the user comes back to an idle
session. It's the single most load-bearing behavioral contract in
the project.

This pairs with the live-wire user story `G6` in
[tests/USER_STORIES.md](../tests/USER_STORIES.md). That story needs
a real `claude` process on a VM to prove the hook wire is alive;
this benchmark proves Coach's reaction logic is correct assuming
the wire works. The two are complementary.

At the HTTP contract level, the property is already covered by
`hook_integration::stop_blocks_then_allows_on_cooldown`. The value
this benchmark adds on top is **a readable, curated scenario that
someone can point at and say "this is what the coach should do when
a user walks away"**. That story is worth writing down, not just
encoding as one assertion in a larger test.

## Expected coach behavior

After the Stop event, the hook response must be:

- `decision == "block"` — the agent is not allowed to end its turn
- `reason` contains the priority list — specifically, the substring
  `"ship the test"` must appear, proving the user's priorities were
  injected into the block message (not a hardcoded fallback)

The `UserPromptSubmit` event itself is passthrough; it just seeds
the session and records activity. We don't assert on it in this
scenario.

## Known limits

- Replay collapses wall-clock time to zero. This scenario covers the
  *first* Stop of a session; the 15-second cooldown is never a
  factor here because there's only one Stop. Cooldown behavior is
  tested separately in `away_stop_cooldown_passes_second`.
- This is a Rules-mode scenario (no LLM). An LLM-mode variant of
  the same story — "when the LLM is available, it evaluates the
  stop using its judgment" — would be a separate scenario and would
  need either a mocked provider or an ignored live-key test.
