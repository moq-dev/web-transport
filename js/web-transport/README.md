# @moq/web-transport

WebTransport for Node.js, powered by native QUIC/HTTP3 via NAPI-RS.

Provides a custom client and server API for setup, then exposes the standard [W3C WebTransport API](https://www.w3.org/TR/webtransport/) for streams, datagrams, and session lifecycle.

See the [package and backend selection guide](../../PACKAGE-SELECTION.md) for alternatives and integration boundaries.

## Installation

```bash
bun add @moq/web-transport
```

## Client

Connect to a WebTransport server using the `Session` class, which implements the W3C `WebTransport` interface:

```ts
import Session from "@moq/web-transport";

const session = new Session("https://example.com:4443/path");
await session.ready;

// Use the standard W3C WebTransport API from here on
```

### Certificate options

```ts
// Skip certificate verification (testing only!)
const session = new Session("https://localhost:4443", {
	serverCertificateDisableVerify: true,
});

// Pin to specific certificate hashes
const session = new Session("https://localhost:4443", {
	serverCertificateHashes: [
		{ algorithm: "sha-256", value: certHash },
	],
});
```

### Subprotocol negotiation

```ts
const session = new Session("https://example.com:4443/path", {
	protocols: ["moqt-16"],
});
await session.ready;
console.log(session.protocol); // "moqt-16" (server-selected)
```

### Idle timeout and keep-alive

A session closes after 30 seconds without network activity, and sends no keep-alives, unless configured. Both options take milliseconds and must be non-negative safe integers:

```ts
const session = new Session("https://example.com:4443/path", {
	idleTimeout: 60_000, // close after 60 seconds of inactivity; 0 disables this endpoint's timeout
	keepAliveInterval: 10_000, // send a keep-alive after 10 seconds without sending; 0 or omitted sends none
});
```

QUIC uses the smaller of both peers' idle timeouts, so disabling one endpoint's timeout leaves the peer's in effect. To keep an otherwise idle session open, set `keepAliveInterval` below the effective idle timeout on either endpoint.

### Polyfill

Install `Session` as the global `WebTransport` for libraries that expect the browser API:

```ts
import { install } from "@moq/web-transport";

install(); // globalThis.WebTransport = Session, plus WebTransportError (no-op if already defined)
```

## Server

Use `Server` to accept incoming connections. Each `Request` can be accepted (returning a W3C `Session`) or rejected:

```ts
import { Server } from "@moq/web-transport";
import fs from "node:fs";

const cert = fs.readFileSync("cert.pem");
const key = fs.readFileSync("key.pem");

const server = Server.bind("[::]:4443", cert, key);

// Stop accepting new connections when shutting down.
process.once("SIGINT", () => server.close());

while (true) {
	const request = await server.accept();
	if (!request) break;

	const url = await request.url;
	console.log("incoming session:", url);

	const session = await request.ok(); // or request.reject(404)

	// Use the standard W3C WebTransport API from here on
	handleSession(session);
}
```

`Server.bind` accepts the same `idleTimeout` and `keepAliveInterval` options, applied to every accepted session:

```ts
const server = Server.bind("[::]:4443", cert, key, { keepAliveInterval: 10_000 });
```

`server.close()` is synchronous and safe to call repeatedly. Pending and future
`accept()` calls resolve to `null`; sessions already accepted remain open until
they are closed separately.

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

### Stream errors

A peer's RESET_STREAM errors the reader, and its STOP_SENDING errors the writer, with a `WebTransportError` whose `source` is `"stream"` and whose `streamErrorCode` is the peer's code. A session failure errors its streams with `source: "session"`. Pass a `WebTransportError` to `abort()` or `cancel()` to send a code of your own:

```ts
import { WebTransportError } from "@moq/web-transport";

await writer.abort(new WebTransportError("cancelled", { streamErrorCode: 7 }));
```

### Closing

Always call `session.close()` when finished and await `session.closed`. The Node wrapper retains native Session handles while its close promise and incoming stream operations are pending. Garbage collecting the JavaScript Session does not close those handles, so it does not provide the Rust Session's final-handle cleanup contract.

```ts
// Close gracefully
session.close({ closeCode: 0, reason: "done" });

// Wait for the session to close
const info = await session.closed;
console.log(info.closeCode, info.reason);
```

## Runtime tests

From the repository root, install the Bun tooling and run:

```bash
nix develop --command bun install --frozen-lockfile
nix develop --command bun run --cwd js/web-transport test:runtime
```

This builds the native release artifact, runs the package prepack build to emit
JavaScript and declarations, packs the package, and extracts it into
a temporary `node_modules` directory. The tests import the packaged entry point
under the Nix shell's Node runtime and exercise a local client/server connection,
uni/bi streams, datagrams, close information, and blocked readers waking on close.
One process must exit naturally after cleanup; another exits with native operations
still pending to exercise runtime teardown. A 30-second process deadline catches
leaked handles and reports the last completed phase. Temporary files and timed-out
process groups are removed; no registry publishing is needed. OpenSSL generates a
short-lived test certificate.

The independent Node runtime PR workflow runs this command on Linux for all pull
requests, including changes to shared Rust code and build/workflow files.

`bun run --cwd js/web-transport build` builds the native module and its JavaScript
wrapper. Packing or publishing runs `build:js` through `prepack`, so the package
exports JavaScript and declarations from `dist/` without requiring a TypeScript
loader in Node. Platform-specific native packages and their release layout are
unchanged.

## License

MIT OR Apache-2.0

## Native crypto

The native Quinn backend uses rind through rind-rustls, with an installed process-wide
rustls provider taking precedence. Endpoint keys and Retry integrity always use rind.
Native builds support x86_64, aarch64 and 32-bit ARM; constructing a client or server
returns an error if the CPU lacks a required feature. This also applies with another
TLS provider installed. The native dependency graph no longer builds ring or aws-lc-rs.
