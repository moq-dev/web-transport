[![crates.io](https://img.shields.io/crates/v/web-transport-noq)](https://crates.io/crates/web-transport-noq)
[![docs.rs](https://img.shields.io/docsrs/web-transport-noq)](https://docs.rs/web-transport-noq)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-noq
A wrapper around the Noq API (a Noq fork), abstracting away the annoying HTTP/3 internals.
Provides a QUIC-like API but with web support!

## WebTransport
[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new web API that allows for low-level, bidirectional communication between a client and a server.
It's [available in the browser](https://caniuse.com/webtransport) as an alternative to HTTP and WebSockets.

WebTransport is layered on top of HTTP/3 which itself is layered on top of QUIC.
This library hides that detail and exposes only the QUIC API, delegating as much as possible to the underlying QUIC implementation (Noq, a Noq fork).

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
To use web-transport-noq, first you need to create a [noq::Endpoint](https://docs.rs/noq/latest/noq/struct.Endpoint.html); see the documentation and examples for more information.
The only requirement is that the ALPN is set to `web_transport_noq::ALPN` (aka `h3`).

Afterwards, you use [web_transport_noq::accept](https://docs.rs/web-transport-noq/latest/web_transport_noq/fn.accept.html) (as a server) or [web_transport_noq::connect](https://docs.rs/web-transport-noq/latest/web_transport_noq/fn.connect.html) (as a client) to establish a WebTransport session.
This will take over the QUIC connection and perform the boring HTTP/3 handshake for you.

See the [examples](examples) or [moq-native](https://github.com/moq-dev/moq-rs/blob/main/moq-native/src/quic.rs) for a full setup.

```rust
    // Create a QUIC client.
    let mut endpoint = noq::Endpoint::client("[::]:0".parse()?)?;
    endpoint.set_default_client_config(/* ... */);

    // Connect to the given URL.
    let session = web_transport_noq::connect(&client, Url::parse("https://localhost")?).await?;

    // Create a bidirectional stream.
    let (mut send, mut recv) = session.open_bi().await?;

    // Send a message.
    send.write(b"hello").await?;
```

## API
The `web-transport-noq` API is almost identical to the Noq API, except that [Connection](https://docs.rs/noq/latest/noq/struct.Connection.html) is called [Session](https://docs.rs/web-transport-noq/latest/web_transport_noq/struct.Session.html).

When possible, `Deref` is used to expose the underlying Noq API.
However some of the API is wrapped or unavailable due to WebTransport limitations.
- Stream IDs are not avaialble.
- Error codes are not full VarInts (62-bits) and significantly smaller.
