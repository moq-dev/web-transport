# @moq/web-transport

WebTransport for Node.js, powered by QUIC and HTTP/3.

This package provides both a client-side `WebTransport` polyfill and a server implementation.

## Install

```bash
npm install @moq/web-transport
```

## Client

The `Session` class implements the [W3C WebTransport API](https://www.w3.org/TR/webtransport/).

```ts
import Session from "@moq/web-transport";

const session = new Session("https://example.com:4443");
await session.ready;

// Bidirectional streams
const bidi = await session.createBidirectionalStream();
const writer = bidi.writable.getWriter();
await writer.write(new Uint8Array([1, 2, 3]));
await writer.close();

// Unidirectional streams
const uni = await session.createUnidirectionalStream();
const uniWriter = uni.getWriter();
await uniWriter.write(new Uint8Array([4, 5, 6]));
await uniWriter.close();

// Datagrams
const dgWriter = session.datagrams.writable.getWriter();
await dgWriter.write(new Uint8Array([7, 8, 9]));

// Incoming streams
for await (const recv of session.incomingUnidirectionalStreams) {
	const reader = recv.getReader();
	// read from reader...
}

session.close();
```

### Polyfill

Use `install()` to register `Session` as the global `WebTransport` if one doesn't already exist:

```ts
import { install } from "@moq/web-transport";
install();

// Now use the standard WebTransport API
const session = new WebTransport("https://example.com:4443");
```

### Certificate Options

```ts
// Skip certificate verification (testing only!)
const session = new Session("https://localhost:4443", {
	serverCertificateDisableVerify: true,
});

// Pin to specific certificate hashes
const session = new Session("https://example.com:4443", {
	serverCertificateHashes: [
		{ algorithm: "sha-256", value: hashBuffer },
	],
});
```

## Server

```ts
import { Server } from "@moq/web-transport";
import fs from "node:fs";

const certPem = fs.readFileSync("cert.pem");
const keyPem = fs.readFileSync("key.pem");

const server = Server.bind("[::]:4443", certPem, keyPem);

while (true) {
	const request = await server.accept();
	if (!request) break;

	const url = await request.url;
	console.log("incoming request:", url);

	const session = await request.ok();
	// Use session just like the client-side API
}
```
