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
| `lsof` on the JSONL transcript file | Claude Code closes the file between writes, so it's never open when we look. |
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
`ConnectInfo<SocketAddr>`, then ask the OS which PID owns it. We use the
`netstat2` crate, which is a thin Rust wrapper over the platform-native
APIs:

* **macOS/iOS**: `sysctl(KIPC.PCBLIST)`
* **Linux/Android**: `/proc/net/tcp` + netlink
* **Windows**: `GetExtendedTcpTable` from `iphlpapi.dll`

`netstat2::get_sockets_info` returns a structured list of every TCP
socket the kernel knows about, with the owning PID(s) attached. We
filter for `local_port == peer_port` and pick the first PID that isn't
ours (loopback connections show both ends in the table). The lookup is
**~1.5ms** on macOS and we pay it **once per conversation** (the first
hook of a new `session_id`), then cache `session_id → pid`. Subsequent
hooks for the same conversation are zero-cost.

Why not shell out to `lsof` / `netstat -ano`? The earlier iteration did
exactly that on macOS. We replaced it with `netstat2` because:

* **One implementation across all three platforms** instead of three
  parsers, three formats, three sets of edge cases.
* **30× faster** on the hot path (~1.5ms vs ~50ms for lsof).
* **No string parsing** — the kernel hands back typed data, no parser
  to break when an OS update changes a header line.
* **No runtime dependency** — there is no longer an "is `lsof` on
  PATH?" failure mode that silently degrades Coach to "zero sessions
  ever". `netstat2` is statically linked into the binary.
* **No localization risk** — `netstat -ano` headers are translated on
  non-English Windows installs.

The cost is a build-time dependency on `libclang` for macOS/Linux
developers (`netstat2` uses `bindgen` to generate FFI bindings to BSD
sysctl headers and Linux netlink structs). macOS developers already
have `libclang` via Xcode, which Tauri requires anyway. Windows
developers need nothing — `bindgen` is gated off on Windows in
`netstat2`'s manifest.

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
pid_resolver::resolve(peer_port)  ──►  PID  (~1.5ms netstat2 call)
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

### When resolution fails

Two failure modes:

1. The connection has already torn down by the time we look (would only
   happen if axum somehow handed off the request after the connection
   was closed; not possible with how `ConnectInfo` works in practice).
2. The kernel call itself errors (extremely rare — would mean the
   process has been denied access to its own TCP table).

When resolution fails we **do not** create a phantom session row. We
drop the event from session-list bookkeeping and move on. The next
successful resolution will reattach the row. Better to under-report
briefly than to silently grow ghosts again.

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
* **WSL2 / containerised Claude Code talking to a host-side Coach.**
  TCP traffic from inside WSL2 to a host service goes through a
  Hyper-V vmbus proxy, so the PID Coach sees would be the proxy on the
  Windows side, not the actual Claude Code process. Out of scope until
  someone actually runs that configuration.
