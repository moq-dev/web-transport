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
