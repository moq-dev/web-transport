# [S] Refuse QMux control frames for unopened local streams

## Goal

Rust and TypeScript QMux close the session when the peer sends MAX_STREAM_DATA or STOP_SENDING for a locally initiated stream that has not been opened yet, as RFC 9000 §19.5 and §19.10 require (STREAM_STATE_ERROR). Today both implementations drop these frames silently. TypeScript also gains the STOP_SENDING direction check (refuse it for a receive-only stream) that Rust already has.

## Plan

A locally initiated stream is unopened when its index is at or beyond the next index this side will allocate in that direction. Frames for streams that were opened and have since closed stay ignored. Use the existing errors: Rust returns `Error::InvalidStreamId`, matching the receive-only MAX_STREAM_DATA and STOP_SENDING direction checks; TypeScript throws like its existing receive-only check. Do not add a public error variant.

Read the next-index state without introducing a new lock order; the frame handlers already take the streams lock. Cover uni and bidi, opened, closed and unopened streams, and the TypeScript direction check with scripted-peer session tests in both languages.

This is RFC-mandated validation that well-behaved peers never trigger. It is separate from the draft-02 receive-offset and terminal-size tolerance owned by [QMux draft readiness](/quest/a2/qmux-draft-readiness.md).

## Related

- [QMux initial stream-count bound](/quest/a0/qmux-max-streams-param.md) - the other RFC 9000 peer-input check planned alongside this one
