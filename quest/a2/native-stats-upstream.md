# [S] Establish native per-stream progress metric availability

## Goal

Public native sent/acknowledged stream accessors with verified semantics are available in supported dependency versions for the planned Quinn and Quiche integration.

## Plan

Recheck current Quinn, Noq and Quiche public APIs and upstream issues/PRs. Record existing accessors or a concrete upstream blocker, including whether acknowledged progress is a contiguous prefix and how resets affect it. Advance existing upstream work where authorized; posting outside moq-dev/kixelated requires approval.

Keep this external prerequisite ready rather than blocking initial optional statistics API. If support requires a dependency release or pin update, create that as a separate prerequisite through quest-plan before integration. Do not complete this prerequisite merely after documenting a blocker. Once usable accessors are available, remove this tracker to unblock integration. Do not fork transport internals solely to synthesize approximate delivery counters.

## Related

- [Optional async stream statistics](/quest/a2/stream-stats.md) - initial API is independently useful
