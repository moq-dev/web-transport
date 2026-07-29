# qmux

A Rust implementation of the [QMux protocol](https://www.ietf.org/archive/id/draft-ietf-quic-qmux-02.html) (draft-ietf-quic-qmux-02, negotiating down to draft-01 and draft-00).

QMux brings QUIC's multiplexed streams and flow control to reliable, ordered byte-stream transports like TCP and WebSockets. It allows applications built for QUIC to seamlessly fall back to TCP/TLS when UDP is blocked by network middleboxes, without maintaining separate protocol implementations.

The protocol reuses QUIC frame types and semantics while adapting them for stream-based transports, providing multiplexed streams with flow control and optional unreliable datagrams.

The session implements both `web-transport-trait` surfaces: the async traits and the sans-I/O `poll` traits. Internally the frontends are poll-first state machines built on [kio](https://docs.rs/kio) shared state (the async methods are thin wrappers), while two background tasks drive the transport's read and write halves and a timer task owns the idle timeout and keep-alive ping.

## Install

```toml
[dependencies]
qmux = "0.0.1"
```

### Features

- **`tcp`** - QMux over raw TCP streams
- **`tls`** - QMux over TLS (via `tokio-rustls`)
- **`ws`** - QMux over WebSockets (via `tokio-tungstenite`)
- **`wss`** - QMux over secure WebSockets (WebSocket + TLS)

Default features: `tls`, `wss`

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
