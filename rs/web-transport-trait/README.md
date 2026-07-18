[![crates.io](https://img.shields.io/crates/v/web-transport-trait)](https://crates.io/crates/web-transport-trait)
[![docs.rs](https://img.shields.io/docsrs/web-transport-trait)](https://docs.rs/web-transport-trait)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-trait

[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new browser API powered by [QUIC](https://www.rfc-editor.org/rfc/rfc9000.html) intended as a replacement for WebSockets.
Most importantly, QUIC supports multiple independent data streams.

This crate provides poll-based WebTransport traits with async convenience methods.
Consumers can drive sessions and streams directly with a standard
`std::task::Context`, while callers that prefer async/await use the provided
methods built on `poll_fn`.

-   Quinn: [web-transport-quinn](../web-transport-quinn)
-   Noq: [web-transport-noq](../web-transport-noq)
-   WebSocket / TCP / TLS: [qmux](../qmux)
-   Quiche+Tokio: [web-transport-quiche](../web-transport-quiche)
-   Iroh: [web-transport-iroh](../web-transport-iroh)

If you don't care about the underyling runtime, use the [web-transport](../web-transport) crate.

The poll surface is runtime-independent. Native implementations are `Send` and
`Sync`; those bounds are conditional on WASM so the same traits can also describe
browser transports.
