# @moq/qmux

A [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) polyfill for browsers, using WebSockets as the underlying transport with [QMux](https://www.ietf.org/archive/id/draft-ietf-quic-qmux-00.html) (draft-ietf-quic-qmux-00) framing.

QMux brings QUIC's multiplexed streams and flow control to reliable, ordered byte-stream transports like WebSockets. This allows WebTransport applications to seamlessly fall back when QUIC/UDP is blocked by network middleboxes.

## Install

```bash
npm install @moq/qmux
```

## Usage

Use as a drop-in `WebTransport` replacement:

```ts
import Session from "@moq/qmux"

const transport = new Session("https://example.com/endpoint")
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
