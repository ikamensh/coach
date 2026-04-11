# Refactor Plan

## Goal

Reshape Coach so the main concepts in the product are also clear concepts in the code:

- a real `LlmCoach` entity with a small interface
- session/event handling separated from transport details
- state split into smaller, easier-to-extend pieces
- platform shells (`Tauri`, CLI, HTTP) depending on application services instead of reaching into core logic directly

The focus is maintainability and extension, not clever architecture or performance.

## Target Shape

### 1. Make the coach a first-class entity

Introduce a single high-level boundary for coach behavior:

- `observe_tool_use`
- `evaluate_stop`
- `evaluate_stop_chained`
- `name_session`

This is now started in `coach-core/src/coach.rs` with `LlmCoach`.

Why:

- today the "coach" is spread across `server.rs`, `llm.rs`, `replay.rs`, and session state
- that makes it hard to understand what the coach actually is
- it also makes future changes riskier because transport code and domain behavior are mixed together

### 2. Move live hook flows behind `LlmCoach`

After introducing `LlmCoach`, route real app flows through it:

1. `run_stop` in `coach-core/src/server.rs`
2. observer flow in `run_post_tool_use` / `observer_consumer`
3. session naming in `run_session_namer`
4. replay flow in `coach-core/src/replay.rs`

Status:

- replay is already a good candidate and should use the same boundary
- live stop path is the next concrete step
- observer and naming should follow after stop

### 3. Extract `CoachMemory`

Create a smaller type for coach-specific conversation state, likely holding:

- chain
- last assessment
- last error
- title
- usage / telemetry

This can come out of the current `SessionState.telemetry` area.

Why:

- coach memory is a distinct concept from generic session bookkeeping
- separating it will make future persistence and reset rules easier to reason about

### 4. Introduce a shared session-event model

Define domain events that sit above Claude/Cursor/raw HTTP payloads:

- `SessionStarted`
- `UserPromptSubmitted`
- `PermissionRequested`
- `ToolCompleted`
- `StopRequested`

Then let transport adapters translate raw payloads into these domain events.

Why:

- right now Claude hooks, Cursor hooks, scanner behavior, and replay all feed the state in slightly different shapes
- a shared event model makes the system easier to test and less tied to one transport

Concrete near-term step: the three hook routers (`server.rs`, `codex.rs`, `cursor.rs`) currently re-implement the same set of endpoints with per-source payload parsing. Once domain events exist, they collapse into one router parameterized by source, and the only per-source code is "raw payload → domain event".

### 5. Split `AppState` into smaller roots

The current `AppState` mixes:

- live sessions
- app config
- API tokens
- HTTP client
- test mocking
- sidecar/runtime concerns

Refactor toward smaller pieces such as:

- `SessionRegistry`
- `AppConfig`
- `RuntimeServices`

This should happen after the coach boundary and event model are more stable.

### 6. Create a shared application service layer

Unify mutations currently duplicated across:

- Tauri commands
- HTTP API handlers
- possibly CLI direct paths

Examples:

- set model
- set rules
- set priorities
- set session mode

Why:

- these operations should live in one place
- shells should call services, not reimplement mutation logic

Smaller first step (standalone, can ship before the full service layer): introduce a `state.mutate(|s| { ... })` helper that takes the write lock, runs a closure, and always emits a snapshot. Today every command and hook handler manually calls `emit_snapshot()` after writing — easy to forget, and the compiler can't catch a missing call. The helper makes "mutate-and-notify" the only way to write state, and the full service layer can then be built on top of it.

### 7. Move rules out of `server.rs`

The rules engine and rule data currently live inside the server module.

Extract them into a dedicated area, likely alongside coach logic, so the HTTP layer stops owning business rules.

### 8. Key sessions by `session_id`, not PID

Today `AppState.sessions` is `HashMap<u32, SessionState>` keyed by PID. The hook payload carries a stable `session_id` from Claude/Cursor; we then resolve it to a PID via lsof, parent-walk, and a cache (`server.rs:109-170`). The PID resolver is the most fragile code in the repo, and PIDs are the wrong abstraction in the first place — they get reused, and "a coding session" is what the user actually means.

Switch to `HashMap<SessionId, SessionState>` and store the PID inside the value as best-effort metadata. Lookups become O(1) on a stable identifier instead of a parent walk; nested-Claude and PID-reuse cases stop being routing problems.

Why:

- removes the most fragile code path in the server
- the hook payload already gives us the right key; we're paying lookup cost to map it to the wrong one
- pairs naturally with the `SessionRegistry` split in section 5 — do the rekey as part of carving the registry out, or just before

### 9. Bound the observer queue and tie its lifetime to the session

Each session has a `SessionCoachState.observer_tx: UnboundedSender<…>` created lazily on the first PostToolUse, with a consumer task spawned in the background. There's no explicit shutdown, no backpressure, and no clear answer to "is the consumer running yet?" If the LLM call slows down the queue grows without bound; if a session ends mid-flight, in-flight messages drop on the floor.

Two small changes make this boring:

- bounded channel (capacity 64 is plenty for one developer's tool calls), drop-oldest on overflow with a counter the snapshot can surface
- spawn the consumer task in `SessionState::new()` and `abort()` it in `Drop`, so the channel and the task share the session's lifetime

Why:

- the lazy spawn + unbounded queue is the only piece of session state with murky ownership rules
- pairs with section 2 (route observer flow through `LlmCoach`) — same code, same edit

### 10. Make prompt loading precedence explicit

Smallest item in this doc, included because it bites repeatedly when iterating prompts.

`prompts.rs` reads from `$COACH_PROMPTS_DIR` if set and errors if a file is missing; otherwise it uses embedded copies. With the env var unset, editing `src-tauri/prompts/*.txt` does nothing and there is no warning.

Either:

- in debug builds always read from disk, in release builds always use embedded — drop the env var entirely
- or keep the env var, but at startup print one line of source: `[coach] prompts: embedded` or `[coach] prompts: $COACH_PROMPTS_DIR (6 files loaded)`

Either fix removes the silent failure; the first is simpler.

## Package / Crate Direction

Do **not** split into many crates yet.

First stabilize the seams inside `coach-core`. Once they feel natural, split by boundary:

- `coach-domain`
  sessions, events, snapshots, decisions
- `coach-engine`
  `LlmCoach`, rules engine, prompts, coach memory
- `coach-app`
  scanner, hook server, replay, persistence, adapters
- `src-tauri`
  desktop shell only

This should be a later step, not the next step.

## Practical Order

1. Introduce `LlmCoach` boundary
2. Route live stop path through it
3. Route observer flow through it (tighten observer queue lifecycle while you're there — section 9)
4. Route naming flow through it
5. Route replay through it if not already done
6. Extract `CoachMemory`
7. Introduce `state.mutate(|s| { ... })` helper (mutate-and-notify; small first step from section 6)
8. Introduce domain `SessionEvent` (and collapse the three hook routers onto it — section 4)
9. Rekey sessions by `session_id` (section 8)
10. Split `AppState` into smaller roots
11. Extract shared application services on top of the mutate helper
12. Only then consider crate/package splitting

Independent and ready to grab any time: prompt loading precedence cleanup (section 10).

## Status

Items 1–11 and the standalone prompt-loading cleanup all landed. Only item 12 (crate/package splitting) is still parked — revisit when the seams inside `coach-core` feel natural to split.

Commits that executed the plan:

- `c7dedc5` — section 6 small step: `state::mutate()` helper
- `e1f9f75` — section 9: bounded observer queue + session-scoped consumer lifetime
- `c4d9286` — section 10: `#[cfg(debug_assertions)]` prompt loading, `COACH_PROMPTS_DIR` removed
- `3c9425f` — section 4 / item 8: `SessionEvent` + collapse of the three hook routers
- `341d19b` — section 8 / item 9: rekey sessions by `session_id`, PID demoted to metadata
- `4793e85` — section 5 / item 10: split `AppState` into `sessions` / `config` / `services` roots
- `9dbb736` — section 6 / item 11: shared application service layer

Items 1–6 (the `LlmCoach` boundary and routing stop / observer / naming / replay through it, plus `CoachMemory` extraction) were completed before this plan-execution pass.
