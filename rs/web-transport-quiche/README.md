[![crates.io](https://img.shields.io/crates/v/web-transport-quiche)](https://crates.io/crates/web-transport-quiche)
[![docs.rs](https://img.shields.io/docsrs/web-transport-quiche)](https://docs.rs/web-transport-quiche)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-quiche
A wrapper around the Quiche, abstracting away the annoying API and HTTP/3 internals.
Provides a QUIC-like API but with web support!

## Limitations
This library builds on top of [tokio-quiche](https://docs.rs/tokio-quiche/latest/tokio_quiche/); the "official" Tokio runtime for [quiche](https://github.com/cloudflare/quiche).

[quiche-ez](ez) is a wrapper around `tokio-quiche` that provides an async API.
It tries to cover as many warts as possible but it's still limited by the poor `tokio_quiche` API.
For example, it's only possible to provide a single TLS certificate and it needs to be on disk.

If this library becomes popular, I can spin `quiche-ez` off into a separate crate that performs the Tokio networking itself.
It should result in better performance too.

## WebTransport
[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new web API that allows for low-level, bidirectional communication between a client and a server.
It's [available in the browser](https://caniuse.com/webtransport) as an alternative to HTTP and WebSockets.

WebTransport is layered on top of HTTP/3 which itself is layered on top of QUIC.
This library hides that detail and exposes only the QUIC API, delegating as much as possible to the underlying QUIC implementation (quiche).

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
To use web-transport-quiche, figure it out yourself lul.
