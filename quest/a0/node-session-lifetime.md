# [M] Abandoned Node reader lifetime

## Goal

In `@moq/web-transport`'s Node polyfill, release an abandoned reader's native
stream even while a `read()` is pending, while preserving actively awaited
reads and pipes that retain only their result promises.

## Plan

[PR 486](https://github.com/moq-dev/web-transport/pull/486) implements
`Session.closed` rejection and pending-read cancellation. Automatic pending
reader cleanup remains unresolved.

A weak controller callback allows native stream collection, but also collects
an actively awaited read when only its result promise remains reachable. The
user's promise then hangs after STOP_SENDING is sent. A strong pending pull
preserves ordinary Web Streams behavior, but also retains abandoned readers.
The first local review reproduced both sides of this tradeoff.

Keep active reads safe while deciding the remaining ownership contract. The
existing explicit `reader.cancel()` path releases a pending native read and
preserves its requested STOP_SENDING code.

- Maintainer decision: approve narrowing the reader goal to explicit cancellation, or
  authorize a lifetime design that can distinguish an observed pending read
  from an abandoned one without changing standard read and pipe behavior.
  Recommend preserving standard Web Streams behavior and documenting the
  explicit cancellation boundary. Validate any alternative with collection
  tests for both abandoned readers and actively awaited read/pipe promises.
