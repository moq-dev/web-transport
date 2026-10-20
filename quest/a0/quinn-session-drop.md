# [M] Close Quinn when the final Session handle drops

## Goal

Dropping the final application-visible Quinn Session closes its connection and releases its capsule reader, even when the peer holds CONNECT open. Dropping one clone preserves the other Session handles.

## Plan

Reproduce #428 before changing ownership: the detached capsule reader retains a native connection clone, so native reference counting cannot enforce the router's documented close-on-drop contract. Use a private shared guard owned only by frontend Session clones, following QMux's ownership boundary. Internal tasks, retained streams and underlying native connection handles do not count as Session owners. Apply the same explicit ownership contract to HTTP/3 and raw sessions; document that retained native handles no longer extend a raw Session's lifetime.

Final drop initiates immediate connection teardown and releases the reader without requiring a new spawned task or peer cooperation. Preserve an explicit graceful close already underway, including its first close reason and existing bounded timeout; final drop must neither interrupt capsule delivery nor start a second close. Ensure every terminal path is idempotent and releases task-owned references. Coordinate with open PR #441's runtime/task refactor instead of duplicating or undoing it.

Graceful-close cancellation must also have a synchronous fallback captured before the future is constructed or spawned, so cancellation before its first poll cannot skip cleanup. The fallback directly closes the native connection only if it has not already observed closure, preserves the first close code/reason and observed peer error, and requests capsule-reader cancellation without another spawned task. A timeout inside an async body is not cancellation protection. After runtime shutdown, guarantee local closure/wake requests and resource release when futures are dropped; peer delivery, driver draining and execution of awakened futures require a live runtime.

Add regressions that fail without the fix: one clone drops while another exchanges traffic; the final clone drops while the peer keeps CONNECT open; retained stream/native handles do not prevent final-drop cleanup; raw sessions obey the same ownership rule; explicit close followed by final drop preserves graceful delivery and bounded cleanup. Verify reader termination as well as peer-visible closure. Use controlled time for timeout tests and real time only for real networking. Run focused backend/router checks and the repository gates.

Cover cancellation before the close future's first poll, during a blocked capsule write, while awaiting peer closure, and when a runtime drops a yielded task. On a live runtime, repeat a bounded number of implicit, explicit and cancelled close cycles using a fixed endpoint pair. Release test-owned handles, await bounded endpoint idleness and assert live connection counts return to their initial baseline; verify reader completion or weak ownership too. Do not force-close endpoints to mask leaks. Use deterministic lifecycle checks, not RSS/global task counts or a process-memory soak benchmark. These cases make [Wallace's cancellation and resource-lifetime feedback](https://github.com/moq-dev/web-transport/issues/428#issuecomment-6296458580) explicit without adding a public API or a dependency on the Noq fix.

Keep signatures and wire formats unchanged. Update backend and router lifecycle documentation and affected examples inline; no separate guide, version bump, backport or publication. Prioritize this session-ownership fix ahead of the following credit-ordering work because unbounded connection/task retention has greater impact.

## Closes

- [#428](https://github.com/moq-dev/web-transport/issues/428) - restore final-handle cleanup

## Related

- [Noq close cancellation](/quest/a0/noq-close-cancellation.md) - independently repair the graceful-close fallback and verify repeated cleanup
- [Quiche final Session drop](/quest/a0/quiche-session-drop.md) - independently repair capsule-task ownership
