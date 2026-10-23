# [XS] Bound QMux initial stream-count parameters

## Goal

Rust and TypeScript QMux close the session with a transport-parameter error when the peer's `initial_max_streams_bidi` or `initial_max_streams_uni` exceeds 2^60, as RFC 9000 §18.2 requires. Today these values seed local stream credit unchecked, even though MAX_STREAMS frames above 2^60 are already refused.

## Plan

Validate in the transport-parameter decoders, next to the existing parameter checks: Rust `proto/params.rs` returns the existing `Error::TransportParameter`, and TypeScript `frame.ts` throws like its other TRANSPORT_PARAMETER_ERROR cases. Do not add a public error variant. Reuse the 2^60 bound that the MAX_STREAMS frame decoders already apply rather than defining a second constant.

Cover exactly 2^60 (accepted) and 2^60 + 1 (refused) for both parameters in both languages, plus a session-level test showing the connect fails. No wire encoding changes; well-behaved peers are unaffected.

## Related

- [QMux stream-state checks](/quest/a0/qmux-stream-state.md) - the other RFC 9000 peer-input check planned alongside this one
