# Session tracking

How Coach maps incoming hook events to the Claude Code window they came from,
and why naive matching by `session_id` is broken.

## The problem

A "Claude Code window" — one terminal running `claude` — is what the user
expects to see represented as one row in Coach's session list. Two things
make this hard:

1. The hook payload from Claude Code carries the **conversation** id, not the
   process id. A single window has many conversations over its lifetime —
   `/clear`, `/resume`, and `/compact` all start a new conversation while
   the process keeps running with the same PID.
2. The only file we can scan from outside the process,
   `~/.claude/sessions/<pid>.json`, is written **once at launch** and never
   updated. The `sessionId` it stores is the very first conversation in
   that window, which is almost always already gone by the time the user
   looks at Coach.

So if Coach matches sessions by id, it ends up with two records for every
window the user has touched: one from the file scanner under the dead
launch-time id, and one from hooks under the current id. Each `/clear`
creates yet another orphaned record.

The original implementation also tried to clean these up by id and only
removed records that the scanner had seen — so the hook-side ghosts
lingered until their 1-hour TTL expired.

## What we tried and why it doesn't work

| Approach | Failure mode |
|---|---|
| Match hook → scanner record by `session_id` | The two ids never agree after a `/clear`. Bug we started with. |
| Match by `cwd` | Breaks the moment the user runs two windows in the same project, which is the normal case for the user this is being built for. |
| Mtime of `~/.claude/projects/<cwd>/<sid>.jsonl` | Doesn't tell us which PID owns it. |
| `lsof` on the JSONL | Claude Code closes the file between writes, so it's never open when we look. |
| `transcript_path` field in the hook payload | Same string as `session_id`, same problem. |
| Hook payload field with PID | Doesn't exist. Empirically captured a real payload — Claude Code sends `session_id`, `transcript_path`, `cwd`, `permission_mode`, `hook_event_name`, `tool_name`, `tool_input`, `tool_response`, `tool_use_id`. No process info. |
| Wrap the hook in a `command`-type script that captures `$PPID` | Works, but gives up the clean HTTP design, costs a subprocess per hook, and complicates installation. |

Every application-level signal is either ambiguous or absent.

## First-principles answer

The hook arrives over a TCP connection. The kernel knows which PID opened
that connection. That is the only signal that is **always correct** and
**never ambiguous**, regardless of how many windows are in the same cwd or
how often the user has typed `/clear`.

We extract the peer port from each request via axum's
`ConnectInfo<SocketAddr>`, then ask the OS which PID owns it:

```
lsof -nP -iTCP@127.0.0.1:<peer_port>
```

We filter out our own PID and read off Claude Code's. The lookup costs
~50ms on macOS. We pay it **once per conversation** (the first hook of a
new `session_id`), then cache `session_id → pid`. Subsequent hooks for the
same conversation are zero-cost.

We also register Coach's hook server for the `SessionStart` hook, which
Claude Code fires immediately after `/clear` (and on `startup`, `resume`,
`compact`). This means Coach reacts to a `/clear` on the next event tick,
not on the next tool call.

## Architecture

### Identity: PID is the canonical key

`CoachState.sessions` is `HashMap<u32, SessionState>` keyed by PID. One
window, one entry, end of story. The map mirrors the user's mental model.

`SessionState` carries a `current_session_id: String` field that is updated
on `/clear`. This is what activity log filtering joins against.

### Conversation lifecycle

A `/clear` is genuinely a new conversation, and we treat it as such:

* New `session_id`
* Counters reset (`event_count`, `tool_counts`, `stop_count`,
  `stop_blocked_count`)
* `started_at` reset to now
* `cwd_history` preserved (the window may have moved between projects
  before the `/clear`, and that history is window-scoped, not
  conversation-scoped)
* `pid` and `display_name` preserved — same window

The activity log is global and keyed by hook `session_id`, so the old
conversation's events stay searchable in the global log but vanish from
the per-row view (which filters by `current_session_id`).

### Hook event flow

```
HTTP request to /hook/<event>
        │
        ▼
ConnectInfo<SocketAddr>  ──►  peer port
        │
        ▼
session_id_to_pid cache hit?
        │ no
        ▼
pid_resolver::resolve(peer_port)  ──►  PID  (one lsof call)
        │
        ▼  cache (session_id → pid)
        │
        ▼
CoachState.hook_event_for_pid(pid, session_id, …)
        │
        ├── pid not in sessions yet → create entry
        ├── current_session_id matches → bump counters
        └── current_session_id differs → /clear: replace + reset
```

### Scanner

The scanner becomes very small. It exists only to:

1. Bootstrap: discover live PIDs from `~/.claude/sessions/*.json` so the
   list is populated even before any tool call has fired. The placeholder
   gets `current_session_id = ""` and a `started_at` taken from the file.
2. Garbage collect: detect when a PID is no longer alive (window closed)
   and remove the corresponding session entry plus its cache entries.

The `sessionId` stored inside the file is no longer trusted for anything —
it's the launch-time id, and we only care about the **current**
conversation in each window, which the hooks tell us.

When the first hook arrives for a placeholder, `apply_hook_event`
recognises the empty `current_session_id`, fills it in, sets
`event_count = 1`, and **preserves the scanner's `started_at`** (since
the conversation actually has been running for that long — it just
didn't have a tool call yet). This is distinct from the `/clear` reset
path, which moves `started_at` to "now".

### When lsof fails

Three failure modes:

1. lsof not installed (very rare on macOS/Linux base systems).
2. The connection has already torn down by the time we look (would only
   happen if axum somehow handed off the request after the connection was
   closed; I don't believe this is possible with how `ConnectInfo` works).
3. Permission issues seeing other users' sockets (not relevant — both
   processes run as the same user).

When resolution fails we **do not** create a phantom session row. We log
the event to the activity log under the hook's `session_id` and move on.
The next successful resolution will reattach the row. Better to
under-report briefly than to silently grow ghosts again.

## What we register with Claude Code

```
PermissionRequest  → /hook/permission-request
Stop               → /hook/stop
PostToolUse        → /hook/post-tool-use
SessionStart       → /hook/session-start   (new — required for instant /clear detection)
```

`SessionStart` carries a `source` field: `startup`, `resume`, `clear`,
`compact`. We treat all four as "new conversation in this PID", which is
exactly what they are.

## Out of scope

* **Per-conversation rows.** We could add a "history" view that lists
  every conversation a window has had, with each one's counters. Not
  needed for the current bug, easy to add later — `cwd_history`-style.
* **Cross-platform PID resolution without `lsof`.** A future change could
  use `libproc` on macOS and `/proc/net/tcp` on Linux for sub-millisecond
  lookups. Not worth the dependency until we feel the latency, which
  we won't because we cache.
