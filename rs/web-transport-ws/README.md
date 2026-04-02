# web-transport-ws (Deprecated)

**This crate has been moved to [`qmux`](../qmux/).** The `qmux` crate implements the QMux protocol (draft-ietf-quic-qmux-01) over reliable transports including TCP, TLS, and WebSocket, with backwards compatibility for the legacy `webtransport` wire format.

All Rust types in this crate are thin wrappers or re-exports from `qmux`. Please migrate to `qmux` directly.

## Migration

```diff
-use web_transport_ws::{Client, Server, Session};
+use qmux::ws::{Client, Server};
+use qmux::Session;
```

The `Client`, `Server`, and `Session` APIs are otherwise unchanged.

## JavaScript/TypeScript Polyfill

Although the Rust crate is deprecated, the TypeScript polyfill in `src/` is actively maintained here and supports both the legacy `webtransport` and the new `qmux-01` wire formats. Check if WebTransport is available, otherwise install the polyfill:

```javascript
import { install } from "@moq/web-transport-ws"

// Install the polyfill if needed.
install();

// Now WebTransport is available even in Safari
const transport = new WebTransport("https://example.com/path")
```

## License

MIT OR Apache-2.0
