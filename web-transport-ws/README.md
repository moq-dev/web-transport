# WebTransport Polyfill

A WebTransport polyfill that uses WebSocket as the underlying transport, with implementations in both Rust and TypeScript/JavaScript.

## Wire Protocol

Both implementations use the same QUIC-like frame encoding over WebSocket:
- Variable-length integer encoding (VarInt)
- Stream multiplexing with bidirectional and unidirectional streams
- Frame types: STREAM, RESET_STREAM, STOP_SENDING, etc.

This is a simplified version of [QMux](https://datatracker.ietf.org/doc/draft-opik-quic-qmux/), which might be used in the future.

## JavaScript/TypeScript Usage

Check if WebTransport is available, otherwise install the polyfill:

```javascript
import { install } from "@moq/web-transport-ws"

// Install the polyfill if needed.
install();

// Now WebTransport is available even in Safari
const transport = new WebTransport("https://example.com/path")
```

URLs are automatically rewritten with the WebSocket protocol:
- `https://example.com/path` → `wss://example.com/path`

## Building

### Rust
```bash
cargo build
```

### TypeScript/JavaScript
```bash
npm install
npm run build
```

### Usage
```bash
cargo run --example server
```

```bash
bun examples/client.ts
```

```bash
deno -A --sloppy-imports examples/client.ts
```

#### Bundle TypeScript source to single JavaScript file
```bash
bun build --target=node --outfile=examples/client.js examples/client.ts 
```

```bash
node examples/client.js
```


## License

MIT OR Apache-2.0
