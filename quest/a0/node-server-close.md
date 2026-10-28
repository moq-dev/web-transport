# [S] Node Server.close() returns while accept() is pending

## Goal

In `@moq/web-transport`'s Node polyfill, `Server.close()` completes while an
`accept()` is waiting, and that `accept()` resolves to `null`, so the
README's accept loop shuts down cleanly.

## Plan

Found while fixing [#470](https://github.com/moq-dev/web-transport/pull/470):
`close()` deadlocks behind a pending `accept()` in
`rs/web-transport-node/src/lib.rs`, which the README's accept-loop example
hits. Find what both calls contend on (likely one lock held across the
`accept` await) and let `close()` wake the waiter instead of waiting for it.

Decided 2026-10-28 (maintainer): its own quest in a0, apart from
[Node session and reader lifetime](/quest/a0/node-session-lifetime.md).

Add a loopback regression that fails without the fix: start the README's
accept loop, then close the server and require both to finish.

Public API: none.
