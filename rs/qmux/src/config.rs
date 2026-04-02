use crate::proto::{TransportParams, DEFAULT_MAX_RECORD_SIZE};
use crate::Version;

/// Configuration for a QMux session.
#[derive(Debug, Clone)]
pub struct Config {
    /// Wire format version.
    pub version: Version,
    /// Negotiated application-level protocol (prefix stripped), if any.
    pub protocol: Option<String>,

    /// Max concurrent bidirectional streams the peer can open.
    pub max_streams_bidi: u64,
    /// Max concurrent unidirectional streams the peer can open.
    pub max_streams_uni: u64,
    /// Connection-level receive window in bytes.
    pub max_data: u64,
    /// Per-stream receive window for bidi streams we initiate.
    pub max_stream_data_bidi_local: u64,
    /// Per-stream receive window for bidi streams the peer initiates.
    pub max_stream_data_bidi_remote: u64,
    /// Per-stream receive window for uni streams.
    pub max_stream_data_uni: u64,

    /// Idle timeout in milliseconds (0 = disabled). Only used in QMux01.
    pub max_idle_timeout: u64,
    /// Maximum QMux Record size in bytes (draft-01). Default: 16382.
    pub max_record_size: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: Version::QMux01,
            protocol: None,
            max_streams_bidi: 100,
            max_streams_uni: 100,
            max_data: 1_048_576,                  // 1 MB
            max_stream_data_bidi_local: 262_144,  // 256 KB
            max_stream_data_bidi_remote: 262_144, // 256 KB
            max_stream_data_uni: 262_144,         // 256 KB
            max_idle_timeout: 30_000,             // 30 seconds
            max_record_size: DEFAULT_MAX_RECORD_SIZE,
        }
    }
}

impl Config {
    /// Create a config with default flow control values.
    pub fn new(version: Version, protocol: Option<String>) -> Self {
        Self {
            version,
            protocol,
            ..Default::default()
        }
    }

    /// Convert to wire-format transport parameters.
    pub(crate) fn to_transport_params(&self) -> TransportParams {
        TransportParams {
            max_idle_timeout: self.max_idle_timeout,
            initial_max_data: self.max_data,
            initial_max_stream_data_bidi_local: self.max_stream_data_bidi_local,
            initial_max_stream_data_bidi_remote: self.max_stream_data_bidi_remote,
            initial_max_stream_data_uni: self.max_stream_data_uni,
            initial_max_streams_bidi: self.max_streams_bidi,
            initial_max_streams_uni: self.max_streams_uni,
            max_record_size: self.max_record_size,
        }
    }
}
