# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- *(qmux)* datagram support (RFC 9221) on QMux01: `send_datagram`/`recv_datagram`/`max_datagram_size` on `Session`, negotiated via the `max_datagram_frame_size` transport parameter and configured with `Config::max_datagram_frame_size` (enabled by default). Datagrams are unavailable on QMux00 and the legacy `webtransport` format.

### Changed

- *(qmux)* the `Session` now drives the transport's send and receive halves on separate tasks so a write stalled on transport backpressure no longer blocks reads (or control-frame processing). The writer task pulls the outbound queues in priority order — control ahead of datagrams ahead of bulk stream data (still scheduled by `sendOrder`) — sharing per-stream state with the reader through a lock rather than a bytes channel. `Session::send_datagram` sheds datagrams the moment the transport backs up (the writer stops draining the datagram lane), rather than buffering them behind a stalled socket where they'd arrive stale.
- *(qmux)* dropping the last `Session` clone now closes the connection promptly, tearing down the backend tasks and failing any in-flight stream `write`/`read` with `Error::Closed` (previously teardown waited for the transport to notice). Hold a `Session` clone for as long as you use its streams.
- *(qmux)* the session idle timeout is *deferred* while a write is wedged on transport backpressure: a stalled `send` proves the peer is still there (its receive window is full) but also blocks the keep-alive, so a healthy backpressured connection isn't mistaken for a dead one. The deferral is bounded to one extra idle window, so a peer that dies under backpressure is still reclaimed (rather than hanging until the transport's own, much longer, timeout).

### Breaking

- *(qmux)* the `Transport` trait now splits into `Writer` + `Reader` halves via `Transport::split`; `send`/`recv`/`close` moved onto those halves and `Writer::maintain` was added for timer-driven work (e.g. WebSocket keep-alive). Custom `Transport` implementations must adopt the new shape; the built-in TCP/TLS/Unix/WebSocket transports and `Session::connect`/`accept` are unaffected.
- *(qmux)* the wire-format `proto` module is now internal. Use the session and stream APIs instead of constructing or matching protocol frames directly.
- *(qmux)* `Version` and `KeepAlive` are now `#[non_exhaustive]`, and `StreamId` is now opaque with `StreamId::into_inner` for reading its encoded value. This prevents future protocol versions and configuration fields from causing incidental API breaks.

## [0.3.1](https://github.com/moq-dev/web-transport/compare/qmux-v0.2.0...qmux-v0.3.1) - 2026-06-25

### Added

- *(qmux)* resolve the application protocol during establishment so `Session::protocol` is a synchronous getter; fold establishment into async `connect`/`accept` and add `Config::handshake_timeout` ([#265](https://github.com/moq-dev/web-transport/pull/265))
- *(qmux)* mark `Config`, `Error`, and `Protocol` `#[non_exhaustive]`; replace the `tls` free functions with `tls::Client`/`tls::Server` builders ([#265](https://github.com/moq-dev/web-transport/pull/265))

## [0.2.0](https://github.com/moq-dev/web-transport/compare/qmux-v0.1.3...qmux-v0.2.0) - 2026-06-19

### Added

- *(qmux)* in-band protocol negotiation + Unix socket transport ([#259](https://github.com/moq-dev/web-transport/pull/259))

## [0.0.8](https://github.com/moq-dev/web-transport/compare/qmux-v0.0.7...qmux-v0.0.8) - 2026-05-24

### Other

- bump tokio-quiche, tokio-tungstenite, flume, rcgen, sha2 ([#240](https://github.com/moq-dev/web-transport/pull/240))

## [0.0.7](https://github.com/moq-dev/web-transport/compare/qmux-v0.0.6...qmux-v0.0.7) - 2026-05-21

### Other

- add Keepalive option for WebSocket transports ([#234](https://github.com/moq-dev/web-transport/pull/234))

## [0.0.6](https://github.com/moq-dev/web-transport/compare/qmux-v0.0.5...qmux-v0.0.6) - 2026-04-07

### Other

- Fix @moq/qmux publish and build scripts ([#207](https://github.com/moq-dev/web-transport/pull/207))
- Add NapiClient.disableVerify() and use NAPI tokio runtime ([#205](https://github.com/moq-dev/web-transport/pull/205))

## [0.0.4](https://github.com/moq-dev/web-transport/compare/qmux-v0.0.3...qmux-v0.0.4) - 2026-03-13

### Other

- Add qmux crate and deprecate web-transport-ws ([#191](https://github.com/moq-dev/web-transport/pull/191))
