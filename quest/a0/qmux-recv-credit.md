# [M] Return connection credit for stopped QMux receive streams

## Goal

Stopping or dropping a Rust QMux `RecvStream` with unread data no longer shrinks the connection's `MAX_DATA` window. Per RFC 9000 §4.5, every byte the peer sent on a stopped stream, up to its final size, is credited back to connection flow control exactly once, so abandoned streams can never wedge a healthy session.

Ships on `main` as the next `qmux` 0.6.x patch. No `qmux-v0.5.x` backport. The TypeScript QMux (`@moq/qmux`) already returns this credit.

## Plan

Three paths leak in `rs/qmux/src/session.rs` today; `report_consumed` only runs on application reads:

- Unread bytes at `stop()` or `Drop`: the unread remainder of the current chunk plus whatever is still queued in `inbound_data`.
- STREAM data that arrives after STOP_SENDING. The writer task removes the recv entry when STOP_SENDING is queued, so late data is then either ignored before connection credit is counted (peer-initiated) or counted and never returned (locally initiated).
- The RESET_STREAM gap for an entry that has already been removed; it is never charged or credited.

Decision: keep the stream's receive state after a local stop until its final size is known, as moq-dev/moq's `moq-quic` does. Do not only credit the bytes buffered at stop, because late data would still leak. On stop or drop, credit the unread and queued bytes. While the entry is stopped, count late STREAM data as received and credit it immediately, and credit the RESET_STREAM gap, then retire the entry on FIN or reset. A peer-initiated stream returns its MAX_STREAMS credit when its entry retires, not when the `RecvStream` drops, so a peer that withholds FIN and RESET_STREAM cannot grow the retained state past the stream limit. A duplicate or late frame after retirement must not double-count, which preserves the existing post-terminal ignore and duplicate-RESET rules. The writer task's STOP_SENDING removal and the session task's lookups race today (see the comments at the STREAM and RESET_STREAM handlers), so the stopped state must close that race rather than add a new one.

Regression tests, which fail without the fix: the reporter's scenario, where the peer writes more than half the stream window into each of N streams, each `RecvStream` is dropped unread, and a fresh stream still receives data; late data after STOP_SENDING; and a reset gap after a local stop. The draft-00/01 (non-QMux) paths have no connection flow control and must stay unchanged.

Reference: moq-dev/moq's equivalent tests, `rs/moq-quic/src/connection/mux/tests.rs` `credit_returns_for_unread_and_stopped_streams` and `rs/moq-quic/src/tests/characterize/reset.rs` `stop_returns_connection_credit`.

## Closes

- [#507](https://github.com/moq-dev/web-transport/issues/507) - stopped/dropped RecvStream leaks connection flow-control credit
