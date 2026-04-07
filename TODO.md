# Coach — TODO

State of play after wiring the LLM observer + chained stop evaluator (April 2026).

## Blockers / immediate

- [ ] **OpenAI account is out of quota.** All `live_*` tests in `src-tauri/src/llm.rs` reach OpenAI but fail with `insufficient_quota`. Top up the key in `secrets/llm-providers.md`, or swap in a working one, then run:
  ```sh
  OPENAI_API_KEY=sk-... cargo test --manifest-path src-tauri/Cargo.toml --lib live_ -- --ignored --nocapture
  ```
  Six tests cover: basic round-trip, `previous_response_id` chaining, `json_object` mode, observer chain accumulation, `evaluate_stop_chained` (with and without prior context). All assertions are mechanical (response_id format, JSON parseability) — none depend on stochastic model output.

- [ ] **Verify observer + Stop end-to-end against a real Claude Code session.** Install hooks, put a session in Away, watch one full edit→bash→stop cycle. Things most likely to surface:
  - `response_format: json_object` in `additional_params` may or may not be honored by rig 0.34's Responses API path. If it isn't, `evaluate_stop_chained` will panic on `serde_json::from_str`. Falls back to fixed message via the existing fallback path.
  - Free-form observer responses might be longer than `max_output_tokens=80`. Cheap to fix by lowering the cap or asking the system prompt for "one short sentence."

## Frontend

- [ ] **Model picker: mark observer-incapable providers.** `CoachSnapshot` now exposes `observer_capable_providers: Vec<String>`. The settings page should grey out / badge any provider not in that list (currently only `openai` is in). Without this, a user can pick Gemini and silently lose the observer.
- [ ] **Show `coach_last_assessment` in session detail.** `SessionSnapshot` carries it now; surface it as the coach's "what I think is happening" panel. This is the user-facing payoff for the LLM cost.
- [ ] **Activity log entries**: `Observer/noted` and `Observer/error` are emitted by the observer worker. Make sure they render in the timeline view (might need a small icon/color tweak).

## Persistence

- [ ] **Persist `coach_response_id` and `coach_last_assessment` per session.** Currently in-memory only — app restart drops the chain handle, and the next observer call starts fresh (re-paying the system-prompt setup cost and losing accumulated context). Suggested: `~/.coach/sessions/<session_id>.json` written on every observer/stop update, loaded on startup.
- [ ] **Define chain expiry behavior.** OpenAI Responses API retains state for ~30 days (verify in docs). If a `previous_response_id` is stale, the API returns an error — we should detect and start a fresh chain instead of bubbling the error.

## Design follow-up: `intervene_reasons` dict

The current `StopDecision` is binary: `{allow, message}`. The bigger design we discussed has the LLM emit a dict of reasons that the *mode* filters into actions:

```rust
struct CoachAssessment {
    reasons: HashMap<String, bool>,   // e.g. "block_stop", "rule_violation", "off_track"
    note: Option<String>,
}
```

Then mode-aware handling at the call site:
- Away mode: act on `block_stop`, `off_track`
- Present mode: only surface `rule_violation` to the UI
- Notify mode (future): surface everything to the user

This lets the same LLM analysis serve different mode toggles without re-prompting. Worth doing once the binary version has been validated against a real session.

## Context size / rotation

- [ ] **Chains will grow without bound.** A long agent session = many observer events → server-side state may eventually exceed the model's effective context. Need a rotation strategy:
  - Option A: count observer calls per chain; after N (e.g. 200), summarize the chain by asking the LLM "give me a 500-word digest of everything so far" and start a fresh chain seeded with that digest.
  - Option B: track token usage from `resp.usage.input_tokens` returned on each call, rotate when it crosses a threshold.
- [ ] **Handle PreCompact hook**: if Claude Code rolls its own context, the agent's mental model resets but the coach's doesn't. We should at least log this so the divergence is visible in the activity log.

## Observer model selection

- [ ] **Add `Settings.observer_model` as a separate slot.** Right now the same `model` field is used for both the observer (high-frequency, cheap) and the stop evaluator (rare, can be smarter). They want different speed/quality trade-offs. Default observer to `gpt-5.4-mini` (or whatever's cheapest with good Responses API support), stop evaluator to whatever the user picks.

## Provider parity

- [ ] **Anthropic-backed observer** (no OpenAI dependency). Anthropic doesn't have `previous_response_id` but has first-class prompt caching. With `model.with_automatic_caching()` and a client-side `Vec<Message>`, you get the same economics (cached prefix is ~10% cost). Add Anthropic to `OBSERVER_CAPABLE_PROVIDERS` and route through a separate `chain_anthropic` function. This unblocks users who already have an Anthropic key but not OpenAI.
- [ ] **Gemini context caching is missing in rig 0.34** (TODO comment in `providers/gemini/completion.rs:1945`). Either contribute upstream or accept that Gemini stays observer-incapable.

## Test coverage gaps

- [ ] **Integration test for `handle_post_tool_use` spawning the observer.** Currently I tested the building blocks but not the wiring — the `tokio::spawn(run_observer(...))` path inside the hook handler has no test. Headless router exists (`create_router_headless`); a test could POST a fake event and assert the observer task ran and updated the session.
- [ ] **Stop hook integration test for the chained path.** Same idea: fake an OpenAI provider state, POST to `/hook/stop`, assert the response shape. Would need either a mock LLM client (rig doesn't make this easy) or a `live_*` integration test that spans both layers.
- [ ] **Regression test for the `insufficient_quota` failure mode.** When the LLM call errors, we fall back to fixed `away_message`. Should be a unit test that simulates an LLM failure and verifies the fallback fires (currently tested only by reading the code).

## Cost monitoring

- [ ] **Log per-call token usage.** rig surfaces usage in `resp.usage` — capture it and emit per session totals so we can see what the observer actually costs in practice. Without this, the "see everything" approach is hard to evaluate honestly. Would slot into the snapshot as `session.observer_tokens: { input: u64, output: u64 }`.

## Hook coverage

- [ ] **`UserPromptSubmit` hook**: would let the observer see the user's original ask, not just the agent's actions. Currently the observer learns the goal indirectly from tool patterns. Cheap addition.
- [ ] **`PreCompact` hook**: see "context size / rotation" above.
- [ ] **`SubagentStop` hook**: relevant once Claude Code's subagent flows are common — observer should know when a subagent completes vs the main agent.
