# [L] One conformance suite for every backend's trait implementation

## Goal

The same tests of `web_transport_trait`'s async and poll interfaces run against every native backend, so stream, datagram, and error behavior that callers like moq rely on is covered once and kept consistent across quinn, noq, quiche, iroh, and qmux.

## Plan

Each backend tests its own inherent API, so the trait implementations on top are thinly covered. Line coverage of the stream wrappers, measured with `cargo llvm-cov --workspace --all-features --all-targets` on 2026-11-22: `send.rs` 44% (quinn), 29% (noq), 27% (iroh), 56% (quiche); `recv.rs` 40%, 31%, 33%, 44%. The uncovered lines are mostly the trait impls: `closed`/`poll_closed` reporting a peer's STOP_SENDING (`Stopped` vs `InvalidStopped`), `poll_write` and `poll_read` error mapping, and reads after a reset. The router's native backend (`rs/web-transport/src/quinn.rs`) sits at 35%.

Write the suite once, generic over a backend that yields a connected client/server `Session` pair, under `rs/web-transport-trait` (a test-support module or a dev-only crate), and run it from each backend's tests. Cover open/accept for both stream kinds, data with FIN, reset and stop codes in both directions (including raw QUIC codes and out-of-range ones), session close codes and reasons, datagrams, and both the async and poll surfaces. Record where backends legitimately differ instead of skipping the case. Check coverage of the files above before and after.

## Related

- [qmux-legacy-byte-stream](/quest/m2/qmux-legacy-byte-stream.md) - another QMux transport gap the coverage run found
