# rule_outdated_model_in_write

**Source:** `hook_integration::post_tool_use_triggers_outdated_models_rule`
**Mode:** away (rules)

## What's happening

The agent writes a Python file that hard-codes `gemini-2.0-flash`
as the model identifier. The `outdated_models` rule scans Write
content and flags stale model strings with a suggestion to upgrade.

## Why we have this scenario

Model identifiers rot fast. Code written six months ago may
reference a model that's now deprecated or significantly worse than
its successor. The rule is a nudge — it doesn't block the write,
it injects an `additionalContext` message so the agent sees the
suggestion on its next tool call.

This scenario covers the **Write** tool path. The sibling scenario
`rule_outdated_model_in_edit` covers the Edit path (checking
`new_string`).

## Expected coach behavior

The PostToolUse response for the Write event must include
`hookSpecificOutput.additionalContext` containing the substring
`"gemini-2.0-flash"` (confirming the rule detected the outdated
model) and `"Update to current"` (confirming it offered guidance).

## Known limits

- The rule does substring matching. It would false-positive on a
  comment like `# we migrated away from gemini-2.0-flash`. Good
  enough for a nudge; not suitable for enforcement.
- Only Write, Edit, and Bash tool inputs are scanned. A Read or
  Grep returning outdated model strings wouldn't trigger the rule
  because those tools don't produce *new* code.
