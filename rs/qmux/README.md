# qmux

A Rust implementation of the [QMux protocol](https://www.ietf.org/archive/id/draft-ietf-quic-qmux-02.html) (draft-ietf-quic-qmux-02, negotiating down to draft-01 and draft-00).

QMux brings QUIC's multiplexed streams and flow control to reliable, ordered byte-stream transports like TCP and WebSockets. It allows applications built for QUIC to seamlessly fall back to TCP/TLS when UDP is blocked by network middleboxes, without maintaining separate protocol implementations.

The protocol reuses QUIC frame types and semantics while adapting them for stream-based transports, providing multiplexed streams with flow control and optional unreliable datagrams.

## Install

```toml
[dependencies]
qmux = "0.6"
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

Every entry point takes a `Config` for flow control and timeouts:
`session_config` on `tcp::Config` and `uds::Config`, `with_session_config` on
the rest. The wire-format version and application protocol still come from the
builder or the handshake. The default grants a peer 100 concurrent streams in
each direction; raise `max_streams_bidi` and `max_streams_uni` if your
application holds more streams open at once.

To run QMux over something the built-in modules don't cover, wrap any
`AsyncRead + AsyncWrite` byte stream in `transport::Stream`, or implement
`transport::Transport` yourself, then pass it to `Session::connect` /
`Session::accept`.

## Statistics

`Session::stats()` reports the socket's RTT and delivery rate when available,
falling back to QMux ping RTT. On Linux 4.19 and newer, `TcpStats` also reports
cumulative TCP payload bytes. Sent bytes include retransmissions and TLS,
WebSocket, and QMux framing; TCP/IP headers are excluded. Received bytes count
what TCP delivered, so duplicate retransmissions are excluded. Other platforms
and older kernels report these counters as unavailable.

## Ping responses

Pending QX_PING responses are coalesced into one response carrying the highest
sequence number, as permitted by QMux §4.3. A peer that stops reading cannot
grow a response queue by sending more pings; inbound stream processing continues
while the writer is blocked. Pending responses are discarded during teardown,
and a close frame ends the writer's output.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
