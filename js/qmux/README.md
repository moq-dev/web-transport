# @moq/qmux

A [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) polyfill for browsers, using WebSockets as the underlying transport with [QMux](https://www.ietf.org/archive/id/draft-ietf-quic-qmux-02.html) (draft-ietf-quic-qmux-02, negotiating down to draft-01 and draft-00) framing.

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

### Servers

`Session.accept` runs the other role, over a WebSocket your server has already accepted. The host
performs the upgrade, so it — not this library — chooses the subprotocol; `selectSubprotocol` picks
the value to accept from what the client offered, and the session reads the wire-format version back
off the socket.

```ts
import Session, { selectSubprotocol } from "@moq/qmux"

Deno.serve((req) => {
	const protocol = selectSubprotocol(req.headers.get("sec-websocket-protocol"), {
		protocols: ["moq-lite-04"],
		versions: { "moq-lite-04": null },
	})
	if (!protocol) return new Response("no supported protocol", { status: 400 })

	const { socket, response } = Deno.upgradeWebSocket(req, { protocol })
	const session = Session.accept(socket)
	handle(session) // a WebTransport, same as the client side
	return response
})
```

Any already-accepted `WebSocket` works — `Deno.upgradeWebSocket`, Node's `ws`, or a
`WebSocketStream`. Accepting the upgrade without a negotiated subprotocol falls back to the legacy
`webtransport` wire format, so reject the request when `selectSubprotocol` returns `undefined`
rather than accepting a socket the peer's framing won't match. If your host doesn't expose the
negotiated value on the socket, pass it explicitly as `{ protocol }`.

The result is an ordinary `WebTransport`: the only difference from a client session is which half of
the stream-id space each side owns.

### Detecting a dropped session

`closed` follows the WebTransport contract:

- **Fulfills** with `{ closeCode, reason }` when the session ends gracefully — a `CONNECTION_CLOSE`
  arrived, from either side calling `close()`. That includes a peer that closes because *it* caught
  a protocol violation: it told us why, so you get its close code and reason.
- **Rejects** when the session ends abnormally, with no `CONNECTION_CLOSE`: the socket dropped, the
  peer went idle, or *this* endpoint caught the peer violating the protocol.

```ts
try {
	const info = await transport.closed
	console.log("closed gracefully", info.closeCode, info.reason)
} catch (err) {
	// err is a WebTransportError-shaped SessionError: err.source === "session"
	console.warn("session dropped, reconnecting", err)
}
```

Don't reach for `closeCode` to tell the two apart — close codes are application-defined, so an app
closing with `1006` is indistinguishable from a dropped socket. The settled state is the signal.

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
