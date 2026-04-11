# away_permission_auto_approved

**Source:** USER_STORIES G5
**Mode:** away (rules)

## What's happening

A user set Coach to Away mode and walked away. Their agent then
tries to run a tool that normally needs permission — e.g. `Bash`
to run a shell command. If Coach did nothing, Claude Code would
show a modal asking the user to approve; with no one there to
click it, the agent blocks.

Away mode is precisely the "I'm not here to click modals" mode, so
Coach must answer the permission hook with an **auto-approval** so
the agent can keep working.

The hook sequence:

1. `UserPromptSubmit` — seed the session in Away mode
2. `PermissionRequest(Bash)` — **auto-approved**

## Why we have this scenario

Without this, "Away mode" leaks a UX failure: the user walks away
expecting Coach to handle things, and comes back to an agent stuck
on a permission modal it couldn't answer. Auto-approval is what
makes the Away-mode pitch honest.

This pairs with live-wire story G5 in
[tests/USER_STORIES.md](../tests/USER_STORIES.md). That story is
currently **unreachable** on the live wire because Claude Code
2.1.92 in `-p` (print / non-interactive) mode doesn't POST
`PermissionRequest` over HTTP at all — denied tools just fail
silently inside claude. The contract itself is still worth pinning
down here, because:

- The HTTP path still fires on interactive `claude` runs, which is
  the common case.
- When the upstream regression is fixed, this benchmark is ready
  to verify the behavior without waiting on a VM run.
- The HTTP-level property is already covered by
  `hook_integration::permission_request_auto_approves_in_away_mode`
  — this benchmark is the readable, curated version for
  "what does Coach do in this situation?".

## Expected coach behavior

After the `PermissionRequest` event, the hook response must contain
`hookSpecificOutput.decision.behavior == "allow"`. That's the
exact shape Claude Code expects to mean "skip the modal, treat the
tool as approved". The `expect` key `permission: "allow"` is the
declarative shorthand for that assertion.

The `UserPromptSubmit` event is passthrough; we don't assert on it.

## Known limits

- In `"present"` mode the PermissionRequest would pass through
  (`{}`) and Claude Code would show its normal modal. That
  complementary property is a separate scenario — not included
  yet, because "Present mode does nothing" is the null hypothesis
  and a benchmark for it would mostly test that the runner
  correctly distinguishes modes. Worth adding if we ever catch a
  regression where Present auto-approves by accident.
- `tool_name` in this scenario is `Bash`, but the auto-approval
  doesn't care which tool it is — Coach approves everything in
  Away mode. If a future policy exempts certain tools from
  auto-approval, that policy needs its own scenario.
