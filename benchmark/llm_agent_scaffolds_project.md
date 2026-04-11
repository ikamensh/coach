# llm_agent_scaffolds_project

**Source:** originally `scenario_replay::agent_scaffolds_new_project`.
**Mode:** llm (needs a real API key — `#[ignore]` by default)
**Status:** aspirational — see *Known limits*.

## What's happening

The user wants an Express "hello world" project scaffolded from an
empty directory. Priorities:

- *"Create a new Express.js hello-world server"*
- *"Make sure it starts and responds on port 3000"*

The agent does the full lifecycle:

1. `npm init -y`
2. `npm install express`
3. Writes `index.js` with the server code
4. Runs `node index.js` with a `timeout 3` so the test doesn't hang

Then it stops. Full scaffold, both priorities satisfied.

Hook sequence:

1. `UserPromptSubmit` — the request
2. `PostToolUse(Bash)` — `npm init -y`
3. `PostToolUse(Bash)` — `npm install express`
4. `PostToolUse(Write)` — `index.js`
5. `PostToolUse(Bash)` — `timeout 3 node index.js || true`
6. `Stop` — **should pass through**

## Why we have this scenario

Same role as `llm_agent_completes_task` but with a different shape:
multiple Bash calls, a Write, no Edit. It catches a coach that's
over-reliant on Edit-as-evidence-of-progress — if the observer
prompt implicitly treats "Edit happened" as a signal of work done,
a scenario that uses Write + Bash instead will regress here first.

Together, `completes_task` (Read/Edit/Write/Bash) and
`scaffolds_project` (Bash/Bash/Write/Bash) cover two different
common-shape tool sequences for "work was done".

## Expected coach behavior

Stop → `{}` (passthrough). We don't care about the exact reason
the LLM decided the work is done, just that it did.

## Known limits

Same caveat as `llm_agent_completes_task`: aspirational. Smaller
observer models will occasionally block here. A failure is
evidence of possible over-blocking, not a guarantee. The chain
built from the four PostToolUse events should contain enough
signal for any reasonable model to conclude the work is done, so a
regression here is worth investigating.

This scenario also doesn't assert anything about the *blocked*
case. If every run in a month blocks here, the right response is
probably to tighten the observer prompt, not to loosen this
assertion.
