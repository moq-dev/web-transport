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
- Quiche+Tokio: TODO

If you don't care about the underyling runtime, use the [web-transport](../web-transport) crate.

## Receive defaults

The async and poll receive helpers initialize destination bytes before passing
them to a safe `read` or `poll_read` implementation. Implementations may inspect
the entire destination. Reporting more bytes than the destination holds causes a
panic before buffer advancement. Backends can override the helpers to copy owned
bytes directly or return an existing `Bytes` chunk.

An empty destination returns `Some(0)`, and a zero-sized chunk request returns an
empty chunk, without reading the stream. `None` indicates end of stream; a
successful read of zero bytes does not. Pending or cancelled default reads do not
advance the destination buffer.

The receive regression tests run with the workspace tests. With a nightly
toolchain that includes Miri, also run `cargo miri test -p web-transport-trait` to
check that inspecting a destination never reads uninitialized memory.

## Why Send?
Async traits are awful because you have to choose either `Send` or `!Send`.
We could define a separate `!Send` trait but I currently don't have a use-case for it.

I would like to implement a sans I/O trait at some point for `quiche` and `quinn-proto`.
Again, I just currently don't have a use-case, and I'm not even sure how feasible it would be.
