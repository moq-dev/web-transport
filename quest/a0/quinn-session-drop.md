# [M] Close Quinn when the final Session handle drops

## Goal

Dropping the final application-visible Quinn Session closes its connection and releases its capsule reader, even when the peer holds CONNECT open. Dropping one clone preserves the other Session handles.

## Plan

Reproduce #428 before changing ownership: the detached capsule reader retains a native connection clone, so native reference counting cannot enforce the router's documented close-on-drop contract. Use a private shared guard owned only by frontend Session clones, following QMux's ownership boundary. Internal tasks, retained streams and underlying native connection handles do not count as Session owners. Apply the same explicit ownership contract to HTTP/3 and raw sessions; document that retained native handles no longer extend a raw Session's lifetime.

Final drop initiates immediate connection teardown and releases the reader without requiring a new spawned task or peer cooperation. Preserve an explicit graceful close already underway, including its first close reason and existing bounded timeout; final drop must neither interrupt capsule delivery nor start a second close. Ensure every terminal path is idempotent and releases task-owned references. Coordinate with open PR #441's runtime/task refactor instead of duplicating or undoing it.

Add regressions that fail without the fix: one clone drops while another exchanges traffic; the final clone drops while the peer keeps CONNECT open; retained stream/native handles do not prevent final-drop cleanup; raw sessions obey the same ownership rule; explicit close followed by final drop preserves graceful delivery and bounded cleanup. Verify reader termination as well as peer-visible closure. Use controlled time for timeout tests and real time only for real networking. Run focused backend/router checks and the repository gates.

Keep signatures and wire formats unchanged. Update backend and router lifecycle documentation and affected examples inline; no separate guide, version bump, backport or publication. This work belongs immediately after receive-buffer safety because unbounded connection/task retention has greater impact than the following credit-ordering work.

## Closes

- [#428](https://github.com/moq-dev/web-transport/issues/428) - restore final-handle cleanup
