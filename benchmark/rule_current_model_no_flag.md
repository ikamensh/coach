# rule_current_model_no_flag

**Source:** `hook_integration::post_tool_use_passes_through_current_models`
**Mode:** away (rules)

## What's happening

The agent writes code that uses `gemini-2.5-flash` — a current
model. The outdated_models rule scans the content and finds nothing
to flag.

## Why we have this scenario

The complementary case to `rule_outdated_model_in_write`. Without
it, a regression that makes the rule fire on *every* Write (e.g.
a broken regex) would go unnoticed — the "should fire" scenarios
would still pass. This scenario catches false positives.

## Expected coach behavior

The PostToolUse response must be exactly `{}` (passthrough). No
`additionalContext`, no rule message.

## Known limits

Only checks one current model string. A comprehensive false-
positive suite would test every entry in `OUTDATED_MODELS` against
its suggested replacement — that lives in the unit tests
(`rules::tests::passes_current_models`), not here.
