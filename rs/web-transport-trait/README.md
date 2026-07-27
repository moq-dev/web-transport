[![crates.io](https://img.shields.io/crates/v/web-transport-trait)](https://crates.io/crates/web-transport-trait)
[![docs.rs](https://img.shields.io/docsrs/web-transport-trait)](https://docs.rs/web-transport-trait)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-trait

[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new browser API powered by [QUIC](https://www.rfc-editor.org/rfc/rfc9000.html) intended as a replacement for WebSockets.
Most importantly, QUIC supports multiple independent data streams.

This crate provides a WebTransport trait for Send runtimes.

-   Quinn: [web-transport-quinn](../web-transport-quinn)
-   Noq: [web-transport-noq](../web-transport-noq)
-   WebSocket / TCP / TLS: [qmux](../qmux)
-   Quiche+Tokio: [web-transport-quiche](../web-transport-quiche)
-   Iroh: [web-transport-iroh](../web-transport-iroh)

If you don't care about the underyling runtime, use the [web-transport](../web-transport) crate.

## Polling

Some consumers drive the transport from their own `poll` loop rather than from
async tasks. The two halves of the API meet that need differently, because the
backends do:

-   `SendStream` and `RecvStream` are poll-native. Every backend has a real
    `poll_read`/`poll_write` underneath, so `poll_*` are the trait's required
    methods and the async ones are `poll_fn` wrappers over them. Nothing is
    boxed, and nothing is implemented twice.
-   `Session` is async-native. No backend has a poll form of `accept_uni` or
    `open_bi`; they are multi-step routines over a shared connection. So the
    trait keeps them async and `SessionPoll` adapts them, retaining each
    in-progress operation so a `Pending` poll resumes rather than restarts. That
    matters for `open_uni`/`open_bi`, which claim stream credit before they
    resolve.

`SessionPoll` drives one operation of each kind; clone the session for a second
concurrent `accept` or `open`.

## Why Send?
Async traits are awful because you have to choose either `Send` or `!Send`.
We could define a separate `!Send` trait but I currently don't have a use-case for it.

The `Send`/`Sync` bounds are conditional on WASM (see `MaybeSend`/`MaybeSync`)
so the same traits describe browser transports.
