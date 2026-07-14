//! Wire format types for QMux frame encoding and decoding.

mod frame;
mod params;
mod version;

pub use frame::*;
pub(crate) use params::*;
pub use version::*;

/// Default maximum record size per draft-01 (16382 bytes).
pub use params::DEFAULT_MAX_RECORD_SIZE;

/// Maximum size of a single QMux frame on the wire (type + fields + payload).
/// For draft-00, this is the maximum frame size.
/// For draft-01, this is superseded by max_record_size.
pub const MAX_FRAME_SIZE: usize = 16384;

/// Maximum payload size for a STREAM frame, accounting for frame overhead.
/// Overhead: frame type, stream ID, offset, and length (up to 8 bytes each).
pub const MAX_FRAME_PAYLOAD: usize = MAX_FRAME_SIZE - 32;

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
