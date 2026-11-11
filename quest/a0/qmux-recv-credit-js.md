# [S] Return connection credit for cancelled TypeScript QMux streams

## Goal

Cancelling a TypeScript QMux receive stream (`@moq/qmux`) no longer leaks connection flow-control credit. STREAM data and the RESET_STREAM final-size gap that arrive after cancel are credited back to the connection window exactly once, per RFC 9000 §4.5, so abandoned streams cannot wedge the session.

This is a bug fix under the QMux fix-only policy. It ships as the next `@moq/qmux` patch.

## Plan

Cancel already credits the bytes buffered at that moment, but the cancel callback in `js/qmux/src/session.ts` then deletes the entry from `#recvStreams`. Afterwards `#handleStreamFrame` returns before any accounting: a peer-initiated stream hits the closed-index early return, and a locally initiated one hits "we created the stream, we can skip it". `#handleResetStream` likewise returns without crediting the gap. The peer has counted all of those bytes against `MAX_DATA`.

Decision: mirror the refused-stream path in the same file, which keeps the stream registered until FIN or RESET so `RecvStream.push` rejects late data and the receive path credits it as discarded. On cancel, keep the entry. Count late STREAM data against flow control and credit it immediately, credit the reset gap, then retire the entry on FIN or RESET. Duplicate or post-retirement frames must still not double-count. Add regression tests that fail without the fix: late data after cancel, a reset gap after cancel, and a session that stays usable after many cancelled streams.

## Related

- [QMux receive credit](/quest/a0/qmux-recv-credit.md) - the same fix for the Rust QMux
