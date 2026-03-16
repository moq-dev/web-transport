# qmux

An implementation of the [QMux protocol](https://www.ietf.org/archive/id/draft-ietf-quic-qmux-00.html) (draft-ietf-quic-qmux-00) for Rust and TypeScript.

QMux brings QUIC's multiplexed streams and datagrams to reliable, ordered byte-stream transports like TCP and WebSockets. It allows applications built for QUIC to seamlessly fall back to TCP/TLS when UDP is blocked by network middleboxes, without maintaining separate protocol implementations.

The protocol reuses QUIC frame types and semantics while adapting them for stream-based transports, providing multiplexed streams with flow control and optional unreliable datagrams.

## Rust

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

## TypeScript / JavaScript

```bash
npm install @moq/qmux
```

A WebTransport polyfill that uses WebSockets as the underlying transport. This allows WebTransport applications to work in environments where QUIC/UDP is unavailable, falling back to WebSocket with the QMux framing protocol.

```ts
import Qmux from "@moq/qmux"

// Use as a drop-in WebTransport replacement
const transport = new Qmux("https://example.com/endpoint")
await transport.ready

const stream = await transport.createBidirectionalStream()
```

### Polyfill

Install as a global `WebTransport` polyfill:

```ts
import { install } from "@moq/qmux"

// Only installs if native WebTransport is unavailable
install()

// Now use the standard WebTransport API
const transport = new WebTransport("https://example.com/endpoint")
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
