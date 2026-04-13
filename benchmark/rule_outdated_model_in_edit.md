# rule_outdated_model_in_edit

**Source:** `rules::tests::detects_outdated_gpt_model`
**Mode:** away (rules)

## What's happening

The agent edits a config file, replacing one model string with
another — but the new string is `gpt-4o`, which is outdated. The
rule scans the Edit's `new_string` field and flags it.

## Why we have this scenario

Edit is the most common tool for modifying existing code. If the
rule only checked Write, an agent that edits a model string in
place would slip through. This scenario covers that path.

## Expected coach behavior

The PostToolUse response for the Edit must include
`hookSpecificOutput.additionalContext` containing `"gpt-4o"`.

## Known limits

The rule checks `new_string` only, not `old_string`. An Edit that
*removes* an outdated model won't trigger. That's intentional —
removing an old model is progress, not a problem.
