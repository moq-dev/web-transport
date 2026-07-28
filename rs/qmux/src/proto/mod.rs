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

/// Largest STREAM payload that keeps the encoded frame within `budget` bytes.
///
/// The budget is what the peer accepts for one frame — its `max_record_size` on
/// the record-framed drafts, [`MAX_FRAME_SIZE`] otherwise — so the frame's own
/// header comes out of it: the type, the stream ID, and (QMux only) the offset and
/// the length varint. Header widths are the ones this frame actually encodes, not
/// a worst-case reservation, so a chunk fills the record the peer advertised.
pub fn max_stream_payload(version: Version, budget: u64, id: StreamId, offset: u64) -> u64 {
    // The type is 0x0e/0x0f on QMux and 0x08/0x09 on the legacy binding: one byte
    // either way.
    let header = 1 + varint_size(id.into_inner());
    if !version.is_qmux() {
        // The payload runs to the end of the message, so there is no length varint
        // to reserve.
        return budget.saturating_sub(header);
    }

    let available = budget.saturating_sub(header + varint_size(offset));
    // The length varint describes the payload, so size it from `available`: that
    // bounds every payload that can still fit, and a varint is monotonic in its
    // value, so no larger payload could use a shorter one.
    available.saturating_sub(varint_size(available))
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
