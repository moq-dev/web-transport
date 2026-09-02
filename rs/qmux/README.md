# qmux

A Rust implementation of the [QMux protocol](https://www.ietf.org/archive/id/draft-ietf-quic-qmux-02.html) (draft-ietf-quic-qmux-02, negotiating down to draft-01 and draft-00).

QMux brings QUIC's multiplexed streams and flow control to reliable, ordered byte-stream transports like TCP and WebSockets. It allows applications built for QUIC to seamlessly fall back to TCP/TLS when UDP is blocked by network middleboxes, without maintaining separate protocol implementations.

The protocol reuses QUIC frame types and semantics while adapting them for stream-based transports, providing multiplexed streams with flow control and optional unreliable datagrams.

## Install

```toml
[dependencies]
qmux = "0.5"
```

### Features

- **`tcp`** - QMux over raw TCP streams
- **`uds`** - QMux over Unix domain sockets (Unix only)
- **`tls`** - QMux over TLS (via `tokio-rustls`)
- **`ws`** - QMux over WebSockets (via `tokio-tungstenite`)
- **`wss`** - QMux over secure WebSockets (WebSocket + TLS)

Default features: `tls`, `wss`

## Usage

The crate root holds the transport-independent session API — `Session`,
`SendStream`, `RecvStream`, `Config`, `Version`, and `Error`. Each transport
keeps its own entry point in its own module, so the shared names don't collide:

| Module | Entry point | Feature |
| --- | --- | --- |
| `qmux::tcp` | `tcp::Config` | `tcp` |
| `qmux::uds` | `uds::Config` | `uds` |
| `qmux::tls` | `tls::Client`, `tls::Server` | `tls` |
| `qmux::ws` | `ws::Client`, `ws::Server`, `ws::Upgraded` | `ws` |
| `qmux::transport` | `transport::Stream` | `tcp` or `uds` |

```rust
use qmux::{tcp, Error, Session, Version};

async fn connect(addr: std::net::SocketAddr) -> Result<Session, Error> {
    tcp::Config::new(Version::QMux02)
        .protocols(["moq-lite-04"])
        .connect(addr)
        .await
}
```

To run QMux over something the built-in modules don't cover, wrap any
`AsyncRead + AsyncWrite` byte stream in `transport::Stream`, or implement
`transport::Transport` yourself, then pass it to `Session::connect` /
`Session::accept`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
