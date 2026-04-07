# Coach — TODO

State of play after wiring the LLM observer + chained stop evaluator (April 2026).

## Status

- ✅ **Anthropic chain verified end-to-end** against the live API. All four `live_chain_anthropic_*` / `live_observe_event_chain_anthropic` / `live_evaluate_stop_chained_anthropic` tests pass with a real key. `claude-haiku-4-5-20251001` works (rig's `calculate_max_tokens` doesn't recognize the new naming so we set max_tokens explicitly via `CompletionRequestBuilder::max_tokens()`). Context preservation across turns confirmed: model recalled `PURPLE-OWL-42` from a prior turn over the client-side history with prompt caching.
- ⚠️ **OpenAI chain plumbing verified mechanically but the live tests cannot pass** — the OpenAI key in `secrets/llm-providers.md` now returns HTTP 401 (`invalid_api_key`), one step worse than the previous `insufficient_quota` (HTTP 429). The request shape is correct (we got past 400 / structural validation), it's purely an account issue.

## Blockers / immediate

- [ ] **Replace or top up the OpenAI key.** All 6 `live_*` OpenAI tests in `src-tauri/src/llm.rs` fail with `invalid_api_key` against the secrets-file key. Once a working key is in place, run:
  ```sh
  OPENAI_API_KEY=sk-... cargo test --manifest-path src-tauri/Cargo.toml --lib live_ -- --ignored --nocapture
  ```
  to flip them green.

- [ ] **Verify observer + Stop end-to-end against a real Claude Code session** (with Anthropic, since that's the working path now). Install hooks, set provider to anthropic + `claude-haiku-4-5-20251001`, put a session in Away, watch one full edit→bash→stop cycle. Things most likely to surface:
  - Free-form observer responses might be longer than `max_tokens=80`. Cheap to fix by lowering the cap or asking the system prompt for "one short sentence."
  - Anthropic JSON output for the stop decision occasionally comes back code-fenced. `parse_stop_decision` already handles that via `strip_code_fence`, but real-world variations may need more tolerance.

## Frontend

- [ ] **Model picker: mark observer-incapable providers.** `CoachSnapshot` now exposes `observer_capable_providers: Vec<String>` — currently `["openai", "anthropic"]`. The settings page should grey out / badge any provider not in that list (google, openrouter). Without this, a user can pick Gemini and silently lose the observer.
- [ ] **Show `coach_last_assessment` in session detail.** `SessionSnapshot` carries it now; surface it as the coach's "what I think is happening" panel. This is the user-facing payoff for the LLM cost.
- [ ] **Activity log entries**: `Observer/noted` and `Observer/error` are emitted by the observer worker. Make sure they render in the timeline view (might need a small icon/color tweak).

## Persistence

- [ ] **Persist `coach_chain` and `coach_last_assessment` per session.** Currently in-memory only — app restart drops the chain handle (whether OpenAI `response_id` or Anthropic message history), and the next observer call starts fresh (re-paying setup cost). `CoachChain` derives `Serialize`/`Deserialize` already, so this is mostly file IO. Suggested: `~/.coach/sessions/<pid>.json` written on every observer/stop update, loaded on startup. Note: for Anthropic, the history Vec can grow large — consider bounding or rotating.
- [ ] **Define chain expiry / cache invalidation behavior.**
  - OpenAI: Responses API retains state for ~30 days. If `previous_response_id` is stale we should detect the error and start a fresh chain.
  - Anthropic: prompt cache TTL is 5 minutes by default. If observer events are sparse, the cache cools and the next call pays full input rate. We could detect this from `usage.cache_read_input_tokens` (rig surfaces it) and consider extending TTL via `with_automatic_caching_1h()` for slow sessions.

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

- ✅ **Anthropic-backed observer** — done. `chain_anthropic` uses rig's `with_automatic_caching()` + client-side `Vec<CoachMessage>` history. `OBSERVER_CAPABLE_PROVIDERS` is `["openai", "anthropic"]`. Verified end-to-end against the live API including context preservation across turns.
- [ ] **Gemini support is structurally limited** (verified, see commit message). Google offers no `previous_response_id` analog. `cachedContent` is an immutable static prefix cache — useless for an observer that accumulates new events because every event would force `caches.create`. Even if rig added it, we'd be doing cache thrashing OR resending the full history each call. Reasoning models on Gemini lose `thoughtSignatures` if not round-tripped, so plain history resend re-thinks every turn. Honest answer: Gemini observer is an O(N²) cost path with weaker continuity than the OpenAI/Anthropic options. Park unless someone really wants it.
- [ ] **OpenRouter as a passthrough**: a user with an OpenRouter key but no direct OpenAI/Anthropic key could be routed through OpenRouter's chat completions, but they'd lose stateful chains entirely (OpenRouter is a chat-completions proxy, not Responses API). Probably not worth it.

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
