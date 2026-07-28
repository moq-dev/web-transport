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

Everything is poll-native. `poll_*` are the required methods and the async ones
are provided helpers over them, so nothing is implemented twice.

-   **Streams keep their poll surface in separate traits.** `PollSendStream` and
    `PollRecvStream` hold the `poll_*` methods and the synchronous ones
    (`finish`, `reset`, `stop`, `set_priority`). `SendStream` and `RecvStream`
    sit on top and are entirely provided methods, so a backend implements the
    poll trait and opts in with an empty `impl SendStream for MyStream {}`.
    Overriding is still allowed, and is how a transport that can take ownership
    of a `Bytes` keeps `write_chunk`/`read_chunk` zero-copy.
-   **Sessions split the same way.** `PollSession` holds the `poll_*` methods and
    the synchronous ones (`send_datagram`, `close`, `protocol`,
    `max_datagram_size`, `stats`); `Session` adds the async helpers.

    Its operations take `&mut self`, which is what makes a retained in-progress
    operation safe: each handle has exactly one owner, so a `Pending` poll can
    resume rather than restart — which matters for `open_uni`/`open_bi`, since
    they claim stream credit before they resolve.

    A `&self` poll method holding a retained future would have to either share
    one slot between concurrent callers — the second polls the first's future
    with its own waker, so the first hangs and the resolved stream goes to the
    wrong task — or clone the session on every call. `&mut self` avoids both.

Run operations concurrently by cloning the session; each clone gets independent
poll state. The trait does not require `Clone`, so a session that cannot be
duplicated is still expressible, and an application that wants a freely shared
handle opts into wrapping it rather than every implementation paying for it.

Where a backend already drives an operation from a poll loop it forwards
natively — quinn, quiche, noq and iroh all accept streams through a
`FuturesUnordered`, and qmux polls its channels directly. The rest are async
routines with no poll form, so `SessionOps` holds the retained futures.

## Why Send?
Async traits are awful because you have to choose either `Send` or `!Send`.

`PollSendStream` and `PollRecvStream` carry no `Send` bound at all, which is the
reason they are separate traits rather than a poll surface bolted onto the async
ones. A transport whose streams are pinned to one thread — a thread-per-core
`io_uring` runtime, say — can implement the poll traits and be driven from a poll
loop without ever being `Send`. It just doesn't get the async conveniences, which
have to hand out `Send` futures.

`PollSession` is colorless for the same reason, so a thread-per-core stack is
expressible end to end and not just for its streams. `Session` adds `Send + Sync`
because its helpers hand out `Send` futures; a transport that doesn't want them
implements only `PollSession`.

The `Send`/`Sync` bounds are conditional on WASM (see `MaybeSend`/`MaybeSync`)
so the same traits describe browser transports.
