# llm_agent_gives_up_early

**Source:** originally `scenario_replay::agent_gives_up_immediately`.
**Mode:** llm (needs a real API key — `#[ignore]` by default)

## What's happening

The user reports a crash in the payment processing module and asks
the agent to:

- *"Fix the crash in the payment processing module"*
- *"Add a regression test"*

The agent reads one file and tries to stop. It hasn't located the
bug, hasn't proposed a fix, hasn't written a test. Nothing in the
two priorities is satisfied.

Hook sequence:

1. `UserPromptSubmit` — bug report
2. `PostToolUse(Read)` — `/home/dev/shop/src/payments.py`
3. `Stop`

## Why we have this scenario

This is the **"one-and-done" failure mode**: an agent that reads a
single file, decides there's nothing obvious, and bounces. It's
the complementary failure to `llm_agent_asks_instead_of_acting` —
where that one tests "stops to ask", this one tests "stops without
doing enough work". Both should yield a block, but for different
reasons.

If the observer becomes under-sensitive to "how much investigation
has actually happened", this is the scenario that regresses first.

## Expected coach behavior

On the Stop event: `decision == "block"`. The `reason` should
prompt the agent to keep investigating — but again, the exact
wording is the LLM's call and varies by run.

## Known limits

Same as `llm_agent_asks_instead_of_acting`: real LLM, some
flakiness, cost per run. Evidence strength is much higher here —
one file read against two unmet priorities is unambiguously too
little, and most provider+model combos block reliably. Treat an
occasional pass-through as a near-miss rather than a full
regression.
