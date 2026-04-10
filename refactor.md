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

### 5. Split `CoachState` into smaller roots

The current `CoachState` mixes:

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

### 7. Move rules out of `server.rs`

The rules engine and rule data currently live inside the server module.

Extract them into a dedicated area, likely alongside coach logic, so the HTTP layer stops owning business rules.

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
3. Route observer flow through it
4. Route naming flow through it
5. Route replay through it if not already done
6. Extract `CoachMemory`
7. Introduce domain `SessionEvent`
8. Split `CoachState`
9. Extract shared application services
10. Only then consider crate/package splitting

## Current Step

Right now the next best step is:

- move the live stop path in `coach-core/src/server.rs` onto `LlmCoach`

Why this next:

- it is high-value and narrow
- it touches the real app behavior
- it avoids a large multi-file rewrite
- once stop is on the boundary, observer and naming can follow with the same pattern
