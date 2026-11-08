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

`ws::Client` and `ws::Server` accept a tungstenite `WebSocketConfig` through
`with_config`. On record-framed drafts (QMux01+), they clamp the advertised
`max_record_size` to the WebSocket's frame and message limits (16 MiB and 64 MiB
by default); the advertised datagram limit is also bounded by that record size.
`connect` and `accept` return `Error::TransportParameter` after WebSocket
negotiation, before sending QMux parameters, if the resulting record size is
below the protocol minimum of 16382 bytes. Bare-frame versions (QMux00 and
legacy `webtransport`) ignore the record limit. Their WebSocket limits must
still accommodate the peer's frames: QMux00 permits frames up to 16384 bytes;
legacy `webtransport` negotiates no size bound. A `None` WebSocket limit is
unbounded. For `ws::Upgraded`, the caller must keep `max_record_size` within the
externally configured frame and message limits, which QMux cannot inspect.

`Config::handshake_timeout` bounds the wait for peer transport parameters after
the transport connects, including WebSocket sessions. A silent peer closes the
session with `Error::HandshakeTimeout`; WebSocket builders return before this
exchange, so observe the error through session operations or `Session::closed`.
The default is 10 seconds, zero disables the deadline, and the legacy
`webtransport` format exchanges no parameters and is exempt.

To run QMux over something the built-in modules don't cover, wrap any
`AsyncRead + AsyncWrite` byte stream in `transport::Stream`, or implement
`transport::Transport` yourself, then pass it to `Session::connect` /
`Session::accept`.

## Ping responses

Pending QX_PING responses are coalesced into one response carrying the highest
sequence number, as permitted by QMux §4.3. A peer that stops reading cannot
grow a response queue by sending more pings; inbound stream processing continues
while the writer is blocked. Pending responses are discarded during teardown,
and a close frame ends the writer's output.

## Stream control validation

MAX_STREAM_DATA and STOP_SENDING close the session when they name a receive-only
stream or a locally initiated stream that has not been allocated. Late controls
for previously opened and closed streams are ignored. These checks follow
[RFC 9000 §19.5](https://www.rfc-editor.org/rfc/rfc9000.html#section-19.5) and
[§19.10](https://www.rfc-editor.org/rfc/rfc9000.html#section-19.10); detected
violations use the existing CONNECTION_CLOSE code 1002.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

## Stream statistics

The async stream `stats()` methods report application payload accepted and read,
excluding protocol headers, including partial progress in cancelled helpers.
Sent, acknowledged and contiguous received progress remain unavailable. See the
[metric definitions and example](../web-transport-trait/README.md#stream-statistics).
