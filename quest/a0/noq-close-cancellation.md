# [M] Release Noq sessions when graceful close is cancelled

## Goal

Cancelling a Noq graceful-close future, including before its first poll or during runtime shutdown, requests synchronous local connection closure and waiter wakeups, cancels the capsule reader, and releases task-owned resources. Repeated session create/drop cycles return live connection and reader-task counts to baseline.

## Plan

Reproduce the cancellation gap left after PR #446: explicit close sets the shared first-close error, so the frontend owner defers cleanup to the graceful-close task. That task force-closes only after awaiting its timeout; dropping the future skips this final close. Use [Wallace's report](https://github.com/moq-dev/web-transport/issues/428#issuecomment-6296458580) as the case to verify, not as runtime reproduction evidence.

Construct a private synchronous cleanup owner before constructing or spawning the graceful-close future, then move it into that future. It must own the native connection and reader-cancellation handle without retaining frontend ownership. Invoke the native per-connection close directly when it has not already observed closure, preserving the first explicit close code/reason and any already observed peer error. Request reader cancellation on every terminal path. An endpoint-level close that only queues an event to a driver is not a synchronous fallback. Keep normal capsule delivery and the existing whole-operation deadline, and make cleanup idempotent without spawning replacement tasks.

Add regressions that fail without the fallback: cancel before the close future's first poll, during a flow-control-blocked capsule write, and while awaiting peer closure; drop a runtime with a yielded close task and retain native handles to inspect local closure. Verify waiter wake requests and errors when those futures can be polled, plus eventual reader termination on a live runtime. Use the existing HTTP/3 fixtures and controlled time; real time belongs only in real-network tests.

Use deterministic lifecycle evidence rather than process RSS or global task-count heuristics. Reuse a fixed endpoint pair for a small bounded number of create/drop cycles covering implicit drop, normal explicit close and cancelled close. Release test-owned stream, native and CONNECT handles, await bounded endpoint idleness on the live runtime, and assert live connection counts return to their initial baseline. Pair this with reader completion or weak-ownership checks. Do not close the endpoints to make the assertions pass. A draining connection may remain in endpoint counts until driver cleanup, so distinguish synchronous local closure from eventual baseline restoration.

After runtime shutdown, require local closure/wake requests and resource release when futures are dropped. Peer capsule or CONNECTION_CLOSE delivery, endpoint draining and execution of awakened futures require a live runtime and are not shutdown guarantees. This does not cover forgotten futures, non-yielding tasks, process abort or SIGKILL.

Keep this independently landable from Quinn, at the original Noq session-cleanup priority immediately after Quinn in a0. Keep public signatures, wire encodings and normal close semantics unchanged. Document cancellation behavior inline with lifecycle APIs/examples; no separate guide, process-memory soak benchmark, version bump, backport or publication. Run focused regressions, native/WASM checks as applicable, and the repository gates.

## Related

- [Quinn final Session drop](/quest/a0/quinn-session-drop.md) - apply the same cancellation and repeated-lifetime cases in its existing implementation quest
