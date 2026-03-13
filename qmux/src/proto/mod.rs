//! Wire format types for QMux frame encoding and decoding.

mod frame;
mod version;

pub use frame::*;
pub use version::*;

/// Maximum size of a single QMux frame on the wire (type + fields + payload).
/// This is the default value per the qmux spec; parameter negotiation is TBD.
pub const MAX_FRAME_SIZE: usize = 16384;

/// Maximum payload size for a STREAM frame, accounting for frame overhead.
/// Overhead: frame_type (up to 8) + stream_id (up to 8) + length (up to 8) = 24 bytes.
pub const MAX_FRAME_PAYLOAD: usize = MAX_FRAME_SIZE - 24;
