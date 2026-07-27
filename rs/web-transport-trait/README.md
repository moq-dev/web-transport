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

If you don't care about the underlying runtime, use the [web-transport](../web-transport) crate.

## Polling

Some consumers drive the transport from their own `poll` loop rather than from
async tasks. The two halves of the API meet that need differently, because the
backends do:

-   **Streams are poll-native, in their own traits.** `PollSendStream` and
    `PollRecvStream` hold the `poll_*` methods and the synchronous ones
    (`finish`, `reset`, `stop`, `set_priority`). Every backend has a real
    `poll_read`/`poll_write` underneath, so nothing is boxed. `SendStream` and
    `RecvStream` sit on top and are entirely provided methods — `poll_fn`
    wrappers plus the `_all` helpers — so a backend implements the poll trait and
    opts in with an empty `impl SendStream for MyStream {}`. Nothing is
    implemented twice, and overriding is still allowed where a transport can take
    ownership of a `Bytes` and keep `write_chunk`/`read_chunk` zero-copy.
-   **Sessions are async-native.** No backend has a poll form of `accept_uni` or
    `open_bi`; they are multi-step routines over a shared connection. So the
    trait keeps them async and `SessionPoll` adapts them, retaining each
    in-progress operation so a `Pending` poll resumes rather than restarts. That
    matters for `open_uni`/`open_bi`, which claim stream credit before they
    resolve. `SessionPoll` drives one operation of each kind; clone the session
    for a second concurrent `accept` or `open`.

## Why Send?
Async traits are awful because you have to choose either `Send` or `!Send`.

`PollSendStream` and `PollRecvStream` carry no `Send` bound at all, which is the
reason they are separate traits rather than a poll surface bolted onto the async
ones. A transport whose streams are pinned to one thread — a thread-per-core
`io_uring` runtime, say — can implement the poll traits and be driven from a poll
loop without ever being `Send`. It just doesn't get the async conveniences, which
have to hand out `Send` futures.

`Session` still requires `Send + Sync`, so a fully thread-per-core stack isn't
expressible yet; only the streams are. Splitting `Session` the same way is
possible later, and would be a breaking change of the same shape.

The `Send`/`Sync` bounds are conditional on WASM (see `MaybeSend`/`MaybeSync`)
so the same traits describe browser transports.
