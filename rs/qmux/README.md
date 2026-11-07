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

### TLS crypto

TLS and WSS dependencies enable no ring or aws-lc-rs provider. `tls::Client`
and `tls::Server` require caller-built rustls configs; their provider, trust
store, and client authentication settings are preserved. The TLS client clones
its config to set QMux ALPNs, leaving the caller's original unchanged.

For WSS without an explicit `ws::Client::with_connector`, QMux uses native
certificate roots and the process's installed rustls provider, otherwise rind
through rind-rustls. It does not install a process-wide provider. Root-loading
or configuration errors fail the connection. Plain WebSockets do not load roots
or select a TLS provider.

The `wss` feature builds on x86_64, aarch64, and 32-bit ARM and requires Rust
1.90 or newer. `ws::Client::new()` stays infallible; `connect()` returns
`Error::UnsupportedCpu` when rind refuses the CPU. An installed provider or an
explicit connector bypasses the fallback and its runtime CPU check. Code that
previously relied on QMux enabling a rustls provider feature must build its
configs with `rustls::ClientConfig::builder_with_provider` or install a provider.

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

## Ping responses

Pending QX_PING responses are coalesced into one response carrying the highest
sequence number, as permitted by QMux §4.3. A peer that stops reading cannot
grow a response queue by sending more pings; inbound stream processing continues
while the writer is blocked. Pending responses are discarded during teardown,
and a close frame ends the writer's output.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
