# @moq/web-socket-stream

A polyfill for the [`WebSocketStream`](https://github.com/ricea/websocketstream-explainer) API, which exposes a WebSocket as a `{ readable, writable }` pair of streams with backpressure on the writable.

`WebSocketStream` currently ships only in Chromium. This package wraps a plain `WebSocket` to present the same surface (`opened`, `closed`, `close()`) on every platform — browsers, Node, and Bun.

> **Note:** a ponyfill can only *approximate* write backpressure by polling `WebSocket.bufferedAmount` against a high-water mark; only the native API observes the real send buffer. Use [`openWebSocketStream`](#prefer-native) to get the native implementation when it's available.

## Install

```bash
npm install @moq/web-socket-stream
```

## Usage

### Prefer native

`openWebSocketStream` returns the native `WebSocketStream` when present and falls back to this ponyfill otherwise:

```ts
import { openWebSocketStream } from "@moq/web-socket-stream"

const wss = openWebSocketStream("wss://example.com", { protocols: ["my-proto"] })
const { readable, writable, protocol } = await wss.opened

const reader = readable.getReader()
const writer = writable.getWriter()
await writer.ready          // backpressure
await writer.write(new Uint8Array([1, 2, 3]))
```

### Global polyfill

Install as a global `WebSocketStream` if the platform doesn't ship one:

```ts
import { install } from "@moq/web-socket-stream"

// Only installs if native WebSocketStream is unavailable
install()

const wss = new WebSocketStream("wss://example.com")
```

### Node / Bun

Modern Node and Bun expose a global `WebSocket`, which is used automatically. To use a specific implementation (e.g. the [`ws`](https://www.npmjs.com/package/ws) package), inject it — this forces the ponyfill:

```ts
import WebSocket from "ws"
import { WebSocketStream } from "@moq/web-socket-stream"

const wss = new WebSocketStream("wss://example.com", { webSocket: WebSocket, highWaterMark: 32 * 1024 })
```

### Adopting an existing socket

A server has no URL to dial: the socket arrives from an HTTP upgrade, already open and with its
subprotocol negotiated. `adopt` wraps one in place, resolving `opened` without waiting for an
`onopen` that has already fired:

```ts
import { WebSocketStream } from "@moq/web-socket-stream"

const { socket, response } = Deno.upgradeWebSocket(req)
const wss = WebSocketStream.adopt(socket)
const { readable, writable } = await wss.opened
```

Adoption takes ownership of the socket's event handlers, so adopt it before anything else reads from
it — messages delivered beforehand are dropped by the platform rather than buffered.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
