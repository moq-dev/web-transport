# [S] Cancel JS QMux writes blocked on flow control

## Goal

In `js/qmux`, a write that waits for stream or connection send credit rejects as
soon as its stream is aborted locally (`writer.abort()`) or stopped by the peer
(STOP_SENDING). `abort()` settles and sends exactly one RESET_STREAM, and no
STREAM frame follows it. This covers all three send sinks in
`js/qmux/src/session.ts`: outgoing bidi, outgoing uni, and incoming bidi. Rust
qmux and the wire format are out of scope: Rust's `claim_credit` already races
both waits against STOP_SENDING.

## Plan

Decided 2026-11-22 (maintainer, /quest-import of #561):

- ✅ keep the goal above (rejected: only the reporter's fix, drop). The
  reporter's fix closes the stream's `sendCredit`, which leaves a write waiting
  on connection credit hung.
- ✅ cancellable credit waits (rejected: closing the stream's `sendCredit`).
  `Credit.claim()` takes an `AbortSignal` and removes its waiter when the signal
  fires. Each send stream gets one `AbortController`, fired by the writable
  controller's `signal` (local abort) and by `#handleStopSending`, and
  `#claimSendCredit` passes it to both its stream and connection waits.
- ✅ release in a separate quest (rejected: version bump in this PR, wait for
  the next routine release).

Mechanism, confirmed against `main` at `f693110`: per the Streams spec, a
`WritableStream` runs the sink's `abort()` only after the in-flight `write()`
settles, so the sink's RESET_STREAM never goes out while the write waits for
credit. `#handleStopSending` errors the controller, which doesn't settle a
running write, and `#maybeDeleteStreamFlow` closes `sendCredit` only once the
receive side is gone too, which a bidi stream's isn't.

Recommendations:

- After each `await` in `#sendStreamDataWithFlowControl`, check the stream is
  still writable, and return any claimed credit before stopping, so no STREAM
  frame follows the RESET_STREAM that fixed the final size.
- Fold the three duplicated sinks into one helper so the fix lands once.
- The write rejects with the abort reason or STOP_SENDING's `StreamError`.
- Test each sink with a peer that grants a small window, covering both
  triggers and both a stream-credit wait and a connection-credit wait. The
  issue's scripted `FakeSocket` is a good starting point. Assert the write and
  `abort()` settle, and that the sent frames end at one RESET_STREAM.

Caused by: [moq-dev/web-transport#199](https://github.com/moq-dev/web-transport/pull/199)
added the credit waits with no per-stream cancellation.

## Closes

- [#561](https://github.com/moq-dev/web-transport/issues/561) - a blocked write never settles after abort or STOP_SENDING
