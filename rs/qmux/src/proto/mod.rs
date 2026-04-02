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
/// Overhead: frame_type (up to 8) + stream_id (up to 8) + length (up to 8) = 24 bytes.
pub const MAX_FRAME_PAYLOAD: usize = MAX_FRAME_SIZE - 24;
