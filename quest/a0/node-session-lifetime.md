# [S] Node session and reader lifetime

## Goal

In `@moq/web-transport`'s Node polyfill, `Session.closed` rejects with a
`WebTransportError` when a session ends abnormally, as browsers do, and an
abandoned reader's native stream is released even while a `read()` is
pending.

## Plan

Found while fixing [#470](https://github.com/moq-dev/web-transport/pull/470):

- `Session.closed` (`js/web-transport/src/session.ts`) only resolves, so a
  failed connect, a lost connection, or a peer's abnormal close never
  rejects it.
- A pending `read()` keeps its native stream alive after the reader is
  abandoned, because napi-rs keeps an object alive while one of its async
  calls is pending. #470 hit the same cause on the writer side, for
  STOP_SENDING, and fixed it there.

Decided 2026-10-28 (maintainer): one quest for both, since they share the
session and stream teardown path, separate from
[Node Server.close()](/quest/a0/node-server-close.md).

Add loopback regressions that fail without the fix: a failed connect and an
abnormal end each reject `closed` with the right `source` and code; dropping
every reference to a reader with a pending read, while the peer keeps the
stream open, makes the peer see the stream terminated, checked without
relying on GC timing where possible.

Public API: `Session.closed` can now reject, matching the WebTransport spec.
