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

## Stream statistics

`SendStream::stats().await` and `RecvStream::stats().await` collect a fresh payload
snapshot. Each field is an `Option<u64>`: `None` means unavailable, and `Some(0)`
is a measured zero. Defaults return unavailable fields, so existing trait
implementors need no new methods. Collection is fallible and cancellation does
not cancel I/O or consume data. Native futures are `Send`; browser futures need
not be. The poll I/O methods update the same counters as async I/O.

The semantics follow the W3C [send](https://www.w3.org/TR/webtransport/#webtransportsendstreamstats)
and [receive](https://www.w3.org/TR/webtransport/#webtransportreceivestreamstats)
stream dictionaries:

| Field | Meaning |
| --- | --- |
| `bytes_written` | Application payload accepted locally, including partial progress before a helper errors or is cancelled. Acceptance does not prove delivery. |
| `bytes_sent` | Payload transmitted at least once, excluding retransmissions. |
| `bytes_acknowledged` | Contiguous payload prefix acknowledged by the peer transport, ending at the first unacknowledged byte. This does not mean the peer application read it. |
| `bytes_received` | Contiguous received payload prefix, ending at the first missing byte. This includes unread data. |
| `bytes_read` | Payload returned by reads, including completed partial reads inside helpers that later error or are cancelled. Adapter prefetch does not count until returned to the caller. |

All counters exclude WebTransport/HTTP/3/QMux headers and network overhead, and
remain cumulative after finish, EOF, reset or stop. Helpers and Tokio/poll I/O
count the same payload exactly once. Backend-specific unordered reads also count
returned bytes, without claiming contiguous receive progress. Available ordered
metrics satisfy `bytes_acknowledged <= bytes_sent <= bytes_written` and
`bytes_read <= bytes_received`.

| Backend | Local write/read counts | Sent / acknowledged / received progress |
| --- | --- | --- |
| Quinn, Noq, Iroh, Quiche, QMux | Supported | Unavailable |
| Browser | Supported by the Rust adapter | Individual fields exposed by the stream's `getStats()` |
| Other implementors using defaults | Unavailable | Unavailable |

Native stream APIs do not expose the required live delivery measurements.
Connection counters include other streams and overhead; a QMux writer accepting
data does not prove transmission or acknowledgment. Neither is used as a
substitute. Native upstream metric integration is separate work.

Browsers are feature-detected per stream. Missing `getStats` or omitted metrics
leave delivery fields unavailable. A present collector that throws, rejects, or
returns malformed data returns an error, rather than hiding the failure. Browser
numbers outside the nonnegative safe-integer range are rejected. Adapter-local
write/read counts account for queued writes and partially returned browser
chunks, so browser prefetch cannot inflate application reads.

```rust
use web_transport_trait::SendStream;

async fn accepted<S: SendStream>(send: &mut S) -> Result<Option<u64>, S::Error> {
    send.write_all(b"hello").await?;
    let stats = send.stats().await?;
    // Preserve unavailable as None; do not turn it into zero or infer delivery.
    Ok(stats.bytes_written)
}
```

The snapshot structs and default async methods are additive Rust APIs; wire
formats and the Node/UniFFI binding surfaces are unchanged. The platform router
also exposes inherent `stats().await` methods and re-exports both snapshot types.
The native conformance tests share `tests/support/stream_stats.rs`; browser
fixtures run in the existing `just test-browser` CI harness.

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

## Why Send?
Async traits are awful because you have to choose either `Send` or `!Send`.
We could define a separate `!Send` trait but I currently don't have a use-case for it.

I would like to implement a sans I/O trait at some point for `quiche` and `quinn-proto`.
Again, I just currently don't have a use-case, and I'm not even sure how feasible it would be.
