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

The Rust workspace contains these crates under [`rs/`](rs/):

- [web-transport](rs/web-transport) provides a platform-selected API backed by Quinn or the browser WebTransport API.
- [web-transport-trait](rs/web-transport-trait) defines async and poll-based interfaces for sessions and streams on native and WASM targets.
- [web-transport-proto](rs/web-transport-proto) implements the HTTP/3 setup for WebTransport and shared protocol primitives.
- [web-transport-quinn](rs/web-transport-quinn) wraps Quinn with WebTransport session setup.
- [web-transport-noq](rs/web-transport-noq) wraps Noq, a Quinn fork, with WebTransport session setup.
- [web-transport-quiche](rs/web-transport-quiche) wraps Quiche through tokio-quiche with WebTransport session setup.
- [web-transport-iroh](rs/web-transport-iroh) runs WebTransport over iroh connections.
- [web-transport-wasm](rs/web-transport-wasm) wraps the browser WebTransport API.
- [qmux](rs/qmux) implements QMux over TCP, TLS, Unix domain sockets, and WebSockets, including the legacy WebTransport-over-WebSocket wire format.
- [web-transport-node](rs/web-transport-node) exposes the Quinn backend through NAPI-RS for the Node.js package.
- [web-transport-ffi](rs/web-transport-ffi) exposes the Quinn backend through UniFFI for Python, Kotlin, and Swift.

## JavaScript workspace

- [@moq/qmux](js/qmux) implements QMux over WebSockets.
- [@moq/web-socket-stream](js/web-socket-stream) provides a WebSocketStream ponyfill and polyfill.
- [@moq/web-transport](js/web-transport) provides a Node.js WebTransport client and server through the native Node bridge.
- [web-demo](js/web-demo) is the browser demo.

## Language bindings

Built from `rs/web-transport-ffi` via UniFFI; all three release workflows fire from a single `web-transport-ffi-v*` tag.

| Language | Package                                                                      | Source       |
|----------|------------------------------------------------------------------------------|--------------|
| Python   | [`web-transport-rs`](https://pypi.org/project/web-transport-rs/) (PyPI)      | [py/web-transport](py/web-transport) |
| Kotlin   | `dev.moq:web-transport` (Maven Central)                                      | [kt/](kt/)   |
| Swift    | `WebTransportFFI.xcframework.zip` (attached to GitHub Releases)              | [swift/](swift/) |

## Quests

Development plans live in [`quest/`](quest/README.md). Enter `nix develop` to
use the pinned Quest CLI, then run `quest guide` for the workflow. Without
entering the shell, use `nix run .#quest -- guide`.

Restart your agent session after setup to load the quest skills. Use
`$quest-plan` in Codex or `/quest-plan` in Claude Code to plan work, and
`$quest-import` or `/quest-import` to bring in GitHub issues. `quest check`
validates the tree and runs as part of `just check` and CI.
