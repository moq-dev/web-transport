# @moq/web-transport

WebTransport for Node.js, powered by native QUIC/HTTP3 via NAPI-RS.

Provides a custom client and server API for setup, then exposes the standard [W3C WebTransport API](https://www.w3.org/TR/webtransport/) for streams, datagrams, and session lifecycle.

## Installation

```bash
bun add @moq/web-transport
```

## Client

Connect to a WebTransport server using the `Session` class, which implements the W3C `WebTransport` interface:

```ts
import Session from "@moq/web-transport";

const session = new Session("https://example.com:4433/path");
await session.ready;

// Use the standard W3C WebTransport API from here on
```

### Certificate options

```ts
// Skip certificate verification (testing only!)
const session = new Session("https://localhost:4433", {
	serverCertificateDisableVerify: true,
});

// Pin to specific certificate hashes
const session = new Session("https://localhost:4433", {
	serverCertificateHashes: [
		{ algorithm: "sha-256", value: certHash },
	],
});
```

### Polyfill

Install `Session` as the global `WebTransport` for libraries that expect the browser API:

```ts
import { install } from "@moq/web-transport";

install(); // globalThis.WebTransport = Session (no-op if already defined)
```

## Server

Use `NapiServer` and `NapiRequest` to accept incoming connections, then wrap each session to get the W3C API:

```ts
import Session, { NapiServer } from "@moq/web-transport";
import fs from "node:fs";

const cert = fs.readFileSync("cert.pem");
const key = fs.readFileSync("key.pem");

const server = NapiServer.bind("0.0.0.0:4433", cert, key);

while (true) {
	const request = await server.accept();
	if (!request) break;

	const url = await request.url;
	console.log("incoming session:", url);

	const napiSession = await request.ok(); // or request.reject(404)
	const session = new Session(napiSession);

	// Use the standard W3C WebTransport API from here on
	handleSession(session);
}
```

## W3C WebTransport API

Once you have a `Session` (client or server-side), the API follows the [W3C WebTransport spec](https://www.w3.org/TR/webtransport/):

### Bidirectional streams

```ts
// Open a bidirectional stream
const stream = await session.createBidirectionalStream();
const writer = stream.writable.getWriter();
await writer.write(new Uint8Array([1, 2, 3]));
await writer.close();

// Accept incoming bidirectional streams
const reader = session.incomingBidirectionalStreams.getReader();
const { value: incoming } = await reader.read();
const data = await new Response(incoming.readable).arrayBuffer();
```

### Unidirectional streams

```ts
// Open a unidirectional stream
const writable = await session.createUnidirectionalStream();
const writer = writable.getWriter();
await writer.write(new TextEncoder().encode("hello"));
await writer.close();

// Accept incoming unidirectional streams
const reader = session.incomingUnidirectionalStreams.getReader();
const { value: readable } = await reader.read();
const text = await new Response(readable).text();
```

### Datagrams

```ts
// Send datagrams
const writer = session.datagrams.writable.getWriter();
await writer.write(new Uint8Array([0x01, 0x02]));

// Receive datagrams
const reader = session.datagrams.readable.getReader();
const { value: datagram } = await reader.read();
```

### Closing

```ts
// Close gracefully
session.close({ closeCode: 0, reason: "done" });

// Wait for the session to close
const info = await session.closed;
console.log(info.closeCode, info.reason);
```

## License

MIT OR Apache-2.0
