# [S] QMux's legacy wire format breaks on byte-stream transports

## Goal

No QMux session over TCP, TLS, or a Unix socket selects the legacy `webtransport` wire format, which only WebSocket can carry. Today such a session dies with `Error::Short` on its first stream byte.

## Plan

The legacy format's STREAM frames (`0x08`/`0x09`) carry no length: a frame ends where its WebSocket message ends (`Frame::decode_wt` in `rs/qmux/src/proto/frame.rs`). A byte stream has no message boundaries, so `recv_qmux00_frame` in `rs/qmux/src/transport.rs` cannot delimit them and returns `Short`. A two-line reproduction: a `Session::connect`/`accept` pair over `tokio::io::duplex` with `Config::new(Version::WebTransport)`, then one uni stream with data.

Three paths reach it:

- `tls::Client` offers the bare `webtransport` ALPN unless `require_protocol` is set (`alpn::build`), so a server that picks it gets a broken session.
- `tls::Client` and `tls::Server` fall back to `Version::WebTransport` when no ALPN is negotiated (`alpn::parse(None)`).
- `tcp::Config`, `uds::Config`, and `transport::Stream::new` accept `Version::WebTransport` without complaint.

Reject the legacy version at setup on every byte-stream transport, stop offering `webtransport` over TLS, and add a regression test per path; `rs/qmux/tests/tls.rs` holds the TLS cases. This is a bug fix, which QMux still takes while its retirement is planned in moq-dev/moq.

A TLS session with no negotiated ALPN fails with `Error::InvalidProtocol("<none>")`, as `require_protocol` already does: the maintainer's choice (2026-11-23), since a QMux TLS server must configure ALPN (rejected: defaulting to the newest QMux draft, which silently mismatches a peer on another draft).
