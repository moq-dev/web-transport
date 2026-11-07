[![crates.io](https://img.shields.io/crates/v/web-transport-noq)](https://crates.io/crates/web-transport-noq)
[![docs.rs](https://img.shields.io/docsrs/web-transport-noq)](https://docs.rs/web-transport-noq)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-noq
A wrapper around the Noq API (a Quinn fork), abstracting away the annoying HTTP/3 internals.
Provides a QUIC-like API but with web support!

## WebTransport
[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new web API that allows for low-level, bidirectional communication between a client and a server.
It's [available in the browser](https://caniuse.com/webtransport) as an alternative to HTTP and WebSockets.

WebTransport is layered on top of HTTP/3 which itself is layered on top of QUIC.
This library hides that detail and exposes only the QUIC API, delegating as much as possible to the underlying QUIC implementation (Noq, a Quinn fork).

QUIC provides two primary APIs:

## Streams

QUIC streams are ordered, reliable, flow-controlled, and optionally bidirectional.
Both endpoints can create and close streams (including an error code) with no overhead.
You can think of them as TCP connections, but shared over a single QUIC connection.

## Datagrams

QUIC datagrams are unordered, unreliable, and not flow-controlled.
Both endpoints can send datagrams below the MTU size (~1.2kb minimum) and they might arrive out of order or not at all.
They are basically UDP packets, except they are encrypted and congestion controlled.

# Usage

Use `ClientBuilder::new().with_system_roots()?` to build a reusable client,
then call `client.connect(url).await?` to establish a session. Clones share the
client's endpoints. The first connection to each address family binds an
IPv4 `0.0.0.0:0` or IPv6-only `[::]:0` socket. IPv4-mapped IPv6 destinations
use IPv4. Socket setup failures are returned by `connect`, so building a client
does not require IPv6 support or a running async runtime. Both sockets use the
runtime present at construction, or at the first connection if constructed
outside a runtime. Keep that runtime alive while using the client.

For an existing `noq::ClientConfig`, `Client::new(endpoint, config)` keeps
the caller's endpoint and address handling. Set its ALPN to `h3`.

See the [examples](examples) for certificate setup and server construction.

## API
The `web-transport-noq` API is almost identical to the Noq API, except that [Connection](https://docs.rs/noq/latest/noq/struct.Connection.html) is called [Session](https://docs.rs/web-transport-noq/latest/web_transport_noq/struct.Session.html).

When possible, `Deref` is used to expose the underlying Noq API.
However some of the API is wrapped or unavailable due to WebTransport limitations.
- Stream IDs are not available.
- Error codes are not full VarInts (62-bits) and significantly smaller.

Session clones share ownership of the connection. Dropping the final `Session`
closes it immediately, including raw QUIC sessions, even if streams or native
`noq::Connection` handles remain. Keep a `Session` clone while using those handles.
Call `close` explicitly to send a best-effort close capsule; its entire graceful
close is bounded by three round trips (at least 100 ms), including flow-controlled
writes, and continues if the final Session is dropped. The first close wins.

Cancelling the graceful-close task, including before its first poll or during
runtime shutdown, closes the native connection and requests waiter wakeups
synchronously, then cancels the capsule reader. The capsule carries the full
UTF-8 reason; the native fallback carries a prefix of at most 16 bytes at a character
boundary. Already observed native errors are preserved. Peer delivery, endpoint
draining and execution of awakened tasks require a live runtime. Forgotten
futures, non-yielding tasks and process abort are outside this guarantee.
