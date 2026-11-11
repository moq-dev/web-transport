[![crates.io](https://img.shields.io/crates/v/web-transport-trait)](https://crates.io/crates/web-transport-trait)
[![docs.rs](https://img.shields.io/docsrs/web-transport-trait)](https://docs.rs/web-transport-trait)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-trait

[WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API) is a new browser API powered by [QUIC](https://www.rfc-editor.org/rfc/rfc9000.html) intended as a replacement for WebSockets.
Most importantly, QUIC supports multiple independent data streams.

This crate defines WebTransport session and stream traits with async and poll-based
interfaces.

| Implementation | Async traits | Poll traits |
| --- | --- | --- |
| [web-transport-quinn](../web-transport-quinn) | Yes | Yes |
| [web-transport-noq](../web-transport-noq) | Yes | No |
| [web-transport-quiche](../web-transport-quiche) (tokio-quiche) | Yes | Yes |
| [web-transport-iroh](../web-transport-iroh) | Yes | No |
| [web-transport-wasm](../web-transport-wasm) (browser WASM) | Yes | Yes |
| [qmux](../qmux) (WebSocket / TCP / TLS / Unix domain sockets) | Yes | No |

The crate root exposes the async `Session`, `SendStream`, and `RecvStream` traits.
The [`poll` module](https://docs.rs/web-transport-trait/latest/web_transport_trait/poll/)
exposes their poll-based counterparts for callers that drive operations from their
own loop. Their `poll_*` operations use `&mut self`; sessions and streams do not
require `Clone`, `Send`, or `Sync`. They describe the session API; the backend
still owns its networking and runtime integration.

The [web-transport](../web-transport) crate provides a concrete API that selects
Quinn or the browser backend at compile time.

See the [package and backend selection guide](../../PACKAGE-SELECTION.md) for alternatives and integration boundaries.

## Receive defaults

The default async and poll receive helpers initialize at most 8 KiB of destination
bytes before passing them to a safe `read` or `poll_read` implementation. The work
per call stays bounded even when the destination has a large allocation and the
stream makes small reads. Implementations may inspect the entire destination.
Reporting more bytes than the destination holds causes a panic before buffer
advancement. Backends can override the helpers to copy owned
bytes directly or return an existing `Bytes` chunk.

In the default helpers, an empty destination returns `Some(0)`, and a zero-sized
chunk request returns an empty chunk, without reading the stream. `None` indicates
end of stream; a successful read of zero bytes does not. Pending or cancelled default reads do not
advance the destination buffer.

The receive regression tests run with the workspace tests. With a nightly
toolchain that includes Miri, also run `cargo miri test -p web-transport-trait` to
check that inspecting a destination never reads uninitialized memory.

Run `cargo bench -p web-transport-trait --bench receive` to measure initialization
across independent streams, destination capacities, and fragment sizes.

## Send and Sync

`MaybeSend` and `MaybeSync` require `Send` and `Sync` when `target_family` is not
`wasm` and impose no such bounds on the `wasm` target family, including browser
and WASI targets. The async `Session` trait and the shared `Error` trait,
including errors returned by the poll traits, require both bounds. The async stream
traits and the futures returned by the async traits require only `MaybeSend`. This
lets the same interface support native runtimes and browser types.
