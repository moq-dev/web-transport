# [M] Make Noq final Session drop release its transport

## Goal

Reproduce and repair Noq's analogous session-retention risk so only frontend Session clones keep the session alive, with consistent HTTP/3 and raw-session behavior.

## Plan

The source mirrors Quinn's detached capsule reader holding a native connection clone; this is an ownership finding, not yet a runtime reproduction. Establish a failing regression before changing code. If reproduction disproves the finding, record the evidence and abandon the quest rather than adding speculative cleanup.

Give frontend Session clones a private shared final-owner guard that internal tasks, streams and native connection handles do not own. Dropping one clone preserves the others; dropping the final clone immediately tears down the connection and releases the reader, even with retained stream/native handles or a peer keeping CONNECT open. Apply this contract to raw sessions too, documenting the change from native reference-counted lifetime.

Preserve an explicit graceful close already underway and its first reason. Noq currently bounds only the wait for peer closure, leaving capsule writes outside the timeout. Bound the entire graceful-close operation so retaining that close after final drop cannot retain the connection indefinitely. Keep teardown idempotent, and avoid requiring a new spawn during implicit drop.

Cover clone survival, peer-held CONNECT, reader termination, retained handles, raw mode and explicit-close-then-drop behavior. Use controlled time to cover a blocked capsule write and the full close deadline, alongside real-network integration coverage. Run focused Noq checks and the repository gates. Keep signatures and wire formats unchanged; update lifecycle docs/examples inline. No separate guide, version bump, backport or publication. Keep this independently landable from Quinn and Quiche.

## Related

- [Quinn final Session drop](/quest/a0/quinn-session-drop.md) - shared frontend ownership policy
- [Quiche final Session drop](/quest/a0/quiche-session-drop.md) - sibling ownership verification
