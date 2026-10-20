# [M] Release Quiche capsule tasks on final Session drop

## Goal

Reproduce and repair the Quiche HTTP/3 capsule task's retention of its connection-drop guard, so the final frontend Session drop closes the connection while remaining Session clones keep it usable.

## Plan

The HTTP/3 capsule task owns a cloned connection wrapper containing the final-drop guard. This is a source finding; first reproduce the retained connection with a peer that leaves CONNECT open. If the finding is disproved, record the evidence and abandon the quest rather than changing ownership speculatively.

Keep the final-owner guard private and shared only by frontend Session/connection-wrapper clones, excluding background capsule tasks. Streams and underlying native driver handles must not extend the frontend Session lifetime. Preserve raw mode's existing final-wrapper-drop behavior, and make HTTP/3 obey the same boundary. Final drop initiates immediate teardown without peer cooperation or a new spawned task, releases the reader and preserves any explicit close already underway and its first reason.

Add regressions for surviving Session clones, final-drop cleanup with CONNECT held open, reader termination, retained streams/native handles, raw sessions and explicit-close-then-drop behavior. Reproduce the bug before fixing it and prove regression sensitivity. Reuse available test seams and fixtures; this quest is independent of the driver harness and stale-notification work, which exercise different lifecycle outcomes. Run focused Quiche checks and the repository gates.

Keep signatures and wire formats unchanged. Update lifecycle documentation and affected examples inline; no separate guide, version bump, backport or publication. Keep this independently landable from Quinn and Noq.

## Related

- [Quinn final Session drop](/quest/a0/quinn-session-drop.md) - shared frontend ownership policy
- [Quiche lifecycle](/quest/a0/quiche/README.md) - nearby driver coverage with separate ownership
