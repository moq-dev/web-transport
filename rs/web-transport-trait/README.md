[![crates.io](https://img.shields.io/crates/v/web-transport-trait)](https://crates.io/crates/web-transport-trait)
[![docs.rs](https://img.shields.io/docsrs/web-transport-trait)](https://docs.rs/web-transport-trait)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-trait

[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new browser API powered by [QUIC](https://www.rfc-editor.org/rfc/rfc9000.html) intended as a replacement for WebSockets.
Most importantly, QUIC supports multiple independent data streams.

This crate provides a WebTransport trait for Send runtimes.

-   Quinn: [web-transport-quinn](../web-transport-quinn)
-   Noq: [web-transport-noq](../web-transport-noq)
-   WebSocket: [web-transport-ws](../web-transport-ws)
- Quiche+Tokio: TODO

If you don't care about the underyling runtime, use the [web-transport](../web-transport) crate.

## Why Send?
Async traits are awful because you have to choose either `Send` or `!Send`.
We could define a separate `!Send` trait but I currently don't have a use-case for it.

I would like to implement a sans I/O trait at some point for `quiche` and `quinn-proto`.
Again, I just currently don't have a use-case, and I'm not even sure how feasible it would be.
