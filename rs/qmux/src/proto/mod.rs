//! Wire format types for QMux frame encoding and decoding.

mod frame;
mod params;
mod version;

use crate::StreamId;

#[cfg(test)]
mod wire_format_tests;

pub use frame::*;
pub(crate) use params::*;
pub use version::*;

/// Default maximum record size per draft-01 (16382 bytes).
pub use params::DEFAULT_MAX_RECORD_SIZE;

/// Maximum size of a single QMux frame on the wire (type + fields + payload).
///
/// This is draft-00's `max_frame_size`, which bounds the whole frame. The
/// record-framed drafts supersede it with the negotiated `max_record_size`, so it
/// only bounds draft-00 and the legacy WebTransport binding.
pub const MAX_FRAME_SIZE: usize = 16384;

/// Send-only compatibility ceiling for STREAM payloads on the currently
/// supported wire formats.
///
/// Released peers historically enforce this value on receive. A future QMux wire
/// version can remove the ceiling once it cannot negotiate with those peers.
/// Never use this value to validate inbound frames.
pub const MAX_FRAME_PAYLOAD: usize = MAX_FRAME_SIZE - 32;

/// Largest STREAM payload that keeps the encoded frame within `budget` bytes.
///
/// The budget is what the peer accepts for one frame — its `max_record_size` on
/// the record-framed drafts, [`MAX_FRAME_SIZE`] otherwise — so the frame's own
/// header comes out of it: the type, the stream ID, and (QMux only) the offset and
/// the length varint. Header widths are the ones this frame actually encodes, not
/// a worst-case reservation. The session applies any send-only compatibility
/// ceiling after calculating this exact peer budget.
pub fn max_stream_payload(version: Version, budget: u64, id: StreamId, offset: u64) -> u64 {
    // The type is 0x0e/0x0f on QMux and 0x08/0x09 on the legacy binding: one byte
    // either way.
    let header = 1 + varint_size(id.into_inner());
    if !version.is_qmux() {
        // The payload runs to the end of the message, so there is no length varint
        // to reserve.
        return budget.saturating_sub(header);
    }

    max_length_prefixed_payload(budget.saturating_sub(header + varint_size(offset)))
}

/// Largest `n` with `n + varint_size(n) <= available`.
///
/// The length varint's width depends on the payload it describes, so this is a
/// fixed point rather than a subtraction: at `available == 16384`, reserving four
/// bytes (the width of 16384 itself) yields 16380, but 16382 fits — its length
/// still encodes in two. Solve it by asking, for each width, what the largest
/// payload that width can describe *and* leave room for is, then take the best.
fn max_length_prefixed_payload(available: u64) -> u64 {
    /// Largest value each varint width encodes (RFC 9000 §16).
    const VARINT_MAX: [u64; 4] = [63, 16383, (1 << 30) - 1, (1 << 62) - 1];

    let mut best = 0;
    for max in VARINT_MAX {
        let width = varint_size(max);
        if available < width {
            break;
        }
        let candidate = (available - width).min(max);
        // Skip a candidate this width cannot describe: a narrower varint means a
        // smaller width already covered it, exactly.
        if varint_size(candidate) == width && candidate > best {
            best = candidate;
        }
    }
    best
}

/// Number of bytes a QUIC varint occupies when encoding `v`.
pub(crate) const fn varint_size(v: u64) -> u64 {
    if v < (1 << 6) {
        1
    } else if v < (1 << 14) {
        2
    } else if v < (1 << 30) {
        4
    } else {
        8
    }
}

#[cfg(test)]
mod max_stream_payload_tests {
    use super::*;
    use crate::{StreamDir, StreamId};
    use web_transport_proto::VarInt;

    /// The encoded frame must fill `budget` as tightly as the varint widths allow:
    /// one byte more would overrun it, and the payload we return must be the
    /// largest one that doesn't.
    fn assert_tight(budget: u64, id: StreamId, offset: u64) {
        let payload = max_stream_payload(Version::QMux01, budget, id, offset);
        let framed =
            |n: u64| 1 + varint_size(id.into_inner()) + varint_size(offset) + varint_size(n) + n;

        assert!(
            framed(payload) <= budget,
            "budget {budget}: payload {payload} encodes to {}",
            framed(payload)
        );
        assert!(
            framed(payload + 1) > budget,
            "budget {budget}: payload {} also fits, so {payload} under-fills",
            payload + 1
        );
    }

    /// Around each varint boundary the length field changes width, which is where
    /// subtracting the width of `available` (rather than of the payload) leaves
    /// bytes on the table: at a 16387 budget it yields 16380, but 16382 fits.
    #[test]
    fn fills_the_budget_at_varint_boundaries() {
        let id = StreamId::new(0, StreamDir::Uni, true);
        for budget in (60..=70).chain(16_380..=16_392) {
            assert_tight(budget, id, 0);
        }
        assert_eq!(max_stream_payload(Version::QMux01, 16_387, id, 0), 16_382);
    }

    /// A wider stream ID or offset takes its bytes out of the same budget.
    #[test]
    fn accounts_for_wider_header_fields() {
        let id = StreamId::new(0, StreamDir::Uni, true);
        let wide = StreamId::new(1 << 20, StreamDir::Uni, true);
        assert_tight(DEFAULT_MAX_RECORD_SIZE, wide, 1 << 20);
        assert!(
            max_stream_payload(Version::QMux01, DEFAULT_MAX_RECORD_SIZE, wide, 1 << 20)
                < max_stream_payload(Version::QMux01, DEFAULT_MAX_RECORD_SIZE, id, 0)
        );
    }

    /// `max_record_size` is a varint, so a peer may advertise up to 2^62-1. The
    /// payload has to stay inside the budget without overflowing.
    #[test]
    fn handles_the_largest_advertised_record_size() {
        let id = StreamId::new(0, StreamDir::Uni, true);
        assert_tight(VarInt::MAX.into_inner(), id, 0);
    }

    /// The legacy binding has no length field: the payload runs to the end of the
    /// message, so only the type and stream ID come out of the budget.
    #[test]
    fn legacy_binding_reserves_no_length() {
        let id = StreamId::new(0, StreamDir::Uni, true);
        assert_eq!(
            max_stream_payload(Version::WebTransport, MAX_FRAME_SIZE as u64, id, 0),
            MAX_FRAME_SIZE as u64 - 2
        );
    }
}
