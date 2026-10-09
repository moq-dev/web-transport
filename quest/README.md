# WebTransport quests

## Goal

Provide a consistent WebTransport API across native backends, browsers, and language bindings, with reliable, interoperable streams and datagrams.

## Plan

Keep living development plans under `quest/`, ordered by priority horizon. Each leaf quest completes in one PR; epics group related outcomes. Finished quests are deleted and git history preserves them. The acts remain permanent.

The initial backlog prioritizes safety and regression foundations, then binding coverage/configuration/documentation, then optional metrics and measured optimization. External conditions remain visible as ready quests rather than hidden blockers. Existing PRs retain ownership of their implementation work.

Use `quest guide` for the format and workflow, `quest ready` for actionable work, and `quest check` after editing the tree. Plan with `$quest-plan` or `/quest-plan`; import issues with `$quest-import` or `/quest-import`. Feature changes carry their own API reference and examples; the package-selection guide is separate.

## Required

- [Safety and regression foundations](/quest/a0/README.md) - first priority
- [Bindings, configuration and documentation](/quest/a1/README.md) - next priority
- [Observability and measured optimization](/quest/a2/README.md) - later priority
