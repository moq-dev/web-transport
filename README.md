[![crates.io](https://img.shields.io/crates/v/web-transport)](https://crates.io/crates/web-transport)
[![docs.rs](https://img.shields.io/docsrs/web-transport)](https://docs.rs/web-transport)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# WebTransport
[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new web API that allows for low-level, bidirectional communication between a client and a server.
It's [available in the browser](https://caniuse.com/webtransport) as an alternative to HTTP and WebSockets.

WebTransport is layered on top of HTTP/3 which is then layered on top of QUIC.
This library hides that detail and tries to expose only the QUIC API, delegating as much as possible to the underlying QUIC implementation.

QUIC provides two primary APIs:

## Streams

QUIC streams are ordered, reliable, flow-controlled, and optionally bidirectional.
Both endpoints can create and close streams (including an error code) with no overhead.
You can think of them as TCP connections, but shared over a single QUIC connection.

## Datagrams

QUIC datagrams are unordered, unreliable, and not flow-controlled.
Both endpoints can send datagrams below the MTU size (~1.2kb minimum) and they might arrive out of order or not at all.
They are basically UDP packets, except they are encrypted and congestion controlled.

# Crates

This project is broken up into quite a few different crates:

-   [web-transport](web-transport) provides a generic interface, delegating to [web-transport-quinn](web-transport-quinn) or [web-transport-wasm](web-transport-wasm) depending on the platform.
-   [web-transport-quinn](web-transport-quinn) mirrors the [Quinn API](https://docs.rs/quinn/latest/quinn/index.html), abstracting away the HTTP/3 setup.
-   [web-transport-noq](web-transport-noq) mirrors the [Noq API](https://docs.rs/noq/latest/noq/index.html), a Quinn fork with the same surface area.
-   [web-transport-wasm](web-transport-wasm) wraps the [browser API](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API)
-   [web-transport-ffi](rs/web-transport-ffi) exposes the WebTransport client/server through [UniFFI](https://mozilla.github.io/uniffi-rs/) for Python, Kotlin, and Swift.
- [qmux](qmux) implements QMux (draft-ietf-quic-qmux) over TCP/TLS/WebSocket, with backwards compatibility for the legacy WebTransport-over-WebSocket wire format.
- [web-transport-trait](web-transport-trait) defines an async trait, currently implemented by [web-transport-quinn](web-transport-quinn) and [qmux](qmux).
-   [web-transport-proto](web-transport-proto) a bare minimum implementation of HTTP/3 just to establish the WebTransport session.

## Language bindings

Built from `rs/web-transport-ffi` via UniFFI; all three release workflows fire from a single `web-transport-ffi-v*` tag.

| Language | Package                                                                      | Source       |
|----------|------------------------------------------------------------------------------|--------------|
| Python   | [`web-transport-rs`](https://pypi.org/project/web-transport-rs/) (PyPI)      | `py/web-transport` |
| Kotlin   | `dev.moq:web-transport` (Maven Central)                                      | `kt/`        |
| Swift    | `WebTransportFFI.xcframework.zip` (attached to GitHub Releases)              | `swift/`     |
