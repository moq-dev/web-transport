# web-transport-ws (Deprecated)

**This crate has been moved to [`qmux`](../qmux/).** The `qmux` crate implements the QMux protocol (draft-ietf-quic-qmux-00) over reliable transports including TCP, TLS, and WebSocket, with backwards compatibility for the legacy `webtransport` wire format.

All Rust types in this crate are thin wrappers or re-exports from `qmux`. Please migrate to `qmux` directly.

## Migration

```diff
-use web_transport_ws::{Client, Server, Session};
+use qmux::{Client, Server, Session};

 // Client
-let session = Client::new().with_protocol("moq-03").connect("ws://localhost:4443").await?;
+let session = Client::new().with_protocol("moq-03").connect_ws("ws://localhost:4443").await?;

 // Server
-let session = server.accept(socket).await?;
+let session = server.accept_ws(socket).await?;
```

## JavaScript/TypeScript Polyfill

The TypeScript polyfill in `src/` is still maintained here and supports both the legacy `webtransport` wire format and the new `qmux-00` wire format. Check if WebTransport is available, otherwise install the polyfill:

```javascript
import { install } from "@moq/web-transport-ws"

// Install the polyfill if needed.
install();

// Now WebTransport is available even in Safari
const transport = new WebTransport("https://example.com/path")
```

## License

MIT OR Apache-2.0
