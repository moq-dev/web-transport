[![crates.io](https://img.shields.io/crates/v/web-transport-proto)](https://crates.io/crates/web-transport-proto)
[![discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)

# web-transport-proto
The gritty WebTransport protocol implementation.
Not meant to be used directly, but as a dependency for the [Quinn](../web-transport-quinn), [Noq](../web-transport-noq), [Quiche](../web-transport-quiche), and [Iroh](../web-transport-iroh) backends.
[QMux](../qmux) also uses its shared framing primitives, including QUIC variable-length integers.

CONNECT requests and responses accept a HEADERS payload and decoded field section
of at most 16 KiB. Decoded size counts every field line as its name and value
lengths plus 32 bytes, including repeated names. Literal and Huffman expansion
stop at the remaining budget, and an oversized declared HEADERS length is
refused before its payload is buffered. Encoding a request or response whose
field section exceeds the limit is refused before anything is written. All of
these paths report `ConnectError::FieldSectionTooLarge`.

`Settings::enable_webtransport` advertises this bound through
`MAX_FIELD_SECTION_SIZE`. SETTINGS, capsule, and GREASE frames retain their
64 KiB payload bound.

Encoding measures field sections and SETTINGS pairs before writing them directly
into the destination. Async writes allocate one frame buffer at its measured size.
Huffman decoding borrows contiguous encoded strings and coalesces fragmented
ones for the existing decoder. Decoding keeps owned header strings; synchronous decoding borrows the frame, and
async reads buffer only the bounded payload without consuming the next frame.

Run `cargo bench -p web-transport-proto --bench buffering` to measure allocations,
reallocations, requested allocation bytes and encode boundary copies for CONNECT
and SETTINGS. It covers header count, value size, Huffman strings, borrowed and
owned input, and partial async writes. Fixture and runtime setup are excluded.
The CSV records allocation totals and the iteration count without rounding.
`largest_encode_slice` is the largest `put_slice` into caller-provided storage,
not a count of all internal memory copies. Requested bytes include every allocation
and reallocation request, not peak live memory; header map order can affect buffer
growth. Timing includes allocation instrumentation and is diagnostic, not a gate.
The benchmark also runs under the existing `just test` all-targets check.

## CONNECT response headers

`ConnectResponse` uses the same `http::HeaderMap` representation and
`with_header` / `with_headers` builders as `ConnectRequest`. `with_header` and
`HeaderMap::append` preserve repeated field values in their original order for
each name; `with_headers` replaces existing values for names present in its map.
Read repeated values with `headers.get_all(name)` rather than `headers.get(name)`.

```rust
use web_transport_proto::{ConnectResponse, http::{header, HeaderValue, StatusCode}};

let response = ConnectResponse::new(StatusCode::FORBIDDEN)
    .with_header(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))
    .with_header(header::SET_COOKIE, HeaderValue::from_static("a=1; Secure"))
    .with_header(header::SET_COOKIE, HeaderValue::from_static("b=2; Secure"));
```

`status` and `protocol` own `:status` and `wt-protocol`. The encoder also manages
`sec-webtransport-http3-draft: draft02` for legacy peers. Generic response headers
containing either managed ordinary field return `ConnectError::ReservedHeader`,
even when their value agrees with the typed field. Request encoding rejects a
generic `wt-available-protocols` entry for the same reason. Pseudo-headers cannot
be inserted into `HeaderMap`. Encoding validates the entire response before
writing bytes, including the existing 16 KiB encoded and decoded size bounds.
QPACK encoding supports visible ASCII header values, with internal tabs allowed;
unsupported bytes and leading/trailing whitespace return
`ConnectError::InvalidHttpHeaderValue`.

`decode` and `read` return well-formed responses of any status. Native consumers
establish sessions only for 200 and retain the complete response in a structured
rejection error otherwise. Missing, repeated or misplaced status fields, unknown
pseudo-headers, repeated selected-protocol fields, invalid structured protocol
values and malformed ordinary fields return a protocol error. Decoding and
reading consume exactly one HEADERS frame, including rejections and malformed
field sections. Both request and response debug output redact header values by
default, so diagnostic code must deliberately select fields to print.

### Migration and compatibility

This is a breaking Rust API change. Replace `ConnectResponse::OK` with
`ConnectResponse::default()` (or `ConnectResponse::new(StatusCode::OK)`):
`HeaderMap` cannot be initialized in a constant. `ConnectResponse` remains
non-exhaustive and gains `headers`. Code expecting non-success `decode` / `read`
to return `WrongStatus` must inspect the returned `response.status` instead;
`WrongStatus` and the unused `ErrorStatus` protocol error variants are removed,
and a missing status returns `MissingStatus`.

Quinn, Noq and Iroh `ConnectError::ErrorStatus`, and Quiche
`h3::ConnectError::Status`, now contain `Box<ConnectResponse>` rather than a
status code. Match `response.status` for the old value and `response.headers`
for diagnostics. Their server `reject` methods now accept
`impl Into<ConnectResponse>`, so existing status-code calls still work and a
header-bearing response can be passed directly. Quinn, Noq and Iroh wait for
response acknowledgement or the peer to stop reading before releasing the
handshake, preventing a final connection drop from discarding queued headers.
Quiche waits for the rejected stream to retire. All native rejection futures
also complete if the peer terminates the connection; an unresponsive peer can
hold them until the transport idle timeout, so cancel the future to impose an
application deadline.

Request decoding rejects duplicate, unknown and misplaced pseudo-headers.
Repeated `wt-available-protocols` lines are combined as a structured list before
parsing, retaining all offered protocols in order.

The wire format remains standard HTTP/3 HEADERS with static/literal QPACK, no
new extension or setting. Repeated ordinary lines are preserved instead of
collapsed. Malformed response fields and conflicting managed fields are now
rejected. Legacy draft output and field-section limits are unchanged. No package
versions are bumped in this change.
