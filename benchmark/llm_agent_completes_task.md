# llm_agent_completes_task

**Source:** originally `scenario_replay::agent_completes_the_task`.
**Mode:** llm (needs a real API key — `#[ignore]` by default)
**Status:** aspirational — see *Known limits* below.

## What's happening

The user asks for a `/health` endpoint with a test. Priorities:

- *"Add a /health endpoint that returns 200 OK with {status: ok}"*
- *"Write a test for it"*

The agent does everything it should:

1. Reads the existing server (`/home/dev/api-server/src/server.py`)
2. Edits it to add the `/health` route
3. Writes a new test file (`tests/test_health.py`)
4. Runs `pytest` to verify

Then it stops. Both priorities are done.

Hook sequence:

1. `UserPromptSubmit` — the request
2. `PostToolUse(Read)` — look at existing server
3. `PostToolUse(Edit)` — add the `/health` route
4. `PostToolUse(Write)` — create the test file
5. `PostToolUse(Bash)` — run `pytest`
6. `Stop` — **should pass through** (the work is done)

## Why we have this scenario

This is the **positive case** for the LLM coach: a scenario where
Coach should *not* block. Without something in the benchmark that
asserts "and sometimes the coach lets the agent stop", every LLM
tweak would silently bias toward over-blocking with no regression
signal. This scenario is the counterweight — it catches a coach
that becomes over-cautious.

The design also makes this the cheapest "is my LLM prompt
reasonable?" check: if it blocks here, either the prompt is
broken, the chain construction is broken, or the model is too
timid for the task.

## Expected coach behavior

On the Stop event, the response body should be exactly `{}`
(passthrough). The work toward both priorities is visible in the
chain: the `/health` route was added in an Edit, the test was
written in a Write, and `pytest` ran in a Bash call.

Tool events are passthrough; no assertion.

## Known limits

**This is a known-flaky benchmark.** Real LLMs — especially small
fast models used for observer work — can still block here even
though the task is done. The original comment in
`scenario_replay.rs` spelled out why:

> In practice, fire-and-forget observers complete out of order, so
> the chain may not reflect the full story. If the coach blocks
> here, it's because the last-completing observer saw an early
> event (e.g., "Read server.py") and the stop evaluator doesn't
> know tests were already written.

The HTTP runner in this benchmark *does* wait for the observer to
catch up between events (it polls
`telemetry.calls + telemetry.errors` after each PostToolUse), so
the out-of-order problem is mitigated here. But model quality
still matters. Haiku 4.5 and GPT-4.1-mini block here sometimes;
Sonnet 4.5 passes reliably.

Treat a failure here as **evidence** that the coach is too
pessimistic, not proof. Re-run; if it still blocks, inspect the
chain and the assessment in the activity log.

Keep this scenario aspirational: a hard assertion that the coach
allows the stop. If it's passing, that's genuinely good news —
Coach is both strict enough to block the two "give up" / "ask
instead" cases and permissive enough to let a completed task
finish.
