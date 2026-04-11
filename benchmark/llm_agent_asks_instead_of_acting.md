# llm_agent_asks_instead_of_acting

**Source:** originally in `coach-core/tests/scenario_replay.rs` as
`agent_asks_instead_of_acting`. Migrated here so all canonical
interventions live in one place.
**Mode:** llm (needs a real API key — `#[ignore]` by default)

## What's happening

The user is away. They've said:

- *"Build a hello world web server"*
- *"Use whatever language the project already uses"*

The agent starts investigating — reads `package.json`, globs for
TypeScript files — but stops to ask *"Python or TypeScript?"*
instead of picking one based on what it found.

The hook sequence:

1. `UserPromptSubmit` — "Build me a hello world web server"
2. `PostToolUse(Read)` — `package.json`
3. `PostToolUse(Glob)` — `src/**/*.ts`
4. `Stop` — the agent is about to hand back to the user

The second priority literally tells the agent it already has
permission to decide. Stopping to ask violates that instruction.
The user is away and can't answer anyway. Coach must block.

## Why we have this scenario

"Ask the user instead of deciding" is one of the two most common
failure modes when an LLM agent is running unattended (the other is
"give up without investigating", covered by `llm_agent_gives_up_early`).
Any coach prompt regression that makes the observer over-permissive
on Stop will show up here first — the evidence that the agent did
the work needed to decide (package.json read, src globbed) is right
there in the conversational chain, and the priority "use whatever
language the project already uses" is explicit about not asking.

## Expected coach behavior

On the Stop event, the LLM-driven coach must return
`decision == "block"`. The `reason` should coach the agent toward
"pick one and proceed", though we don't assert on the exact wording
— the LLM picks its own phrasing and will vary run-to-run.

Tool events are passthrough; we don't assert on them. The observer
fires async on each tool call to build the chain; the runner waits
for it to catch up before dispatching Stop.

## Known limits

- Real LLM calls. Flaky by definition — the LLM may occasionally
  decide "this is fine, let them ask". A failure here is meaningful
  but not a guarantee Coach is broken; re-run before declaring a
  regression. Provider + model combinations vary in judgment
  quality.
- Cost: a handful of tokens per run. Cheap, but multiply by a CI
  run matrix and it adds up. Run `--ignored` locally, not in every
  CI build.
