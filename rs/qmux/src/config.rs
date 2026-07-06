use std::time::Duration;

use crate::proto::{TransportParams, DEFAULT_MAX_RECORD_SIZE};
use crate::Version;

/// How the application-level protocol is determined for a session.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum Protocol {
    /// No application protocol.
    #[default]
    None,

    /// Advertise these protocols and negotiate one in-band, via the QMux
    /// `application_protocols` transport parameter (preference order).
    ///
    /// For transports without ALPN (TCP, Unix sockets). Both peers must opt in:
    /// receiving the parameter while *not* in this mode is a protocol error.
    /// The agreed protocol is surfaced by
    /// [`Session::protocol`](web_transport_trait::Session::protocol), resolved
    /// by the time [`Session::connect`](crate::Session::connect) /
    /// [`accept`](crate::Session::accept) returns.
    Negotiate(Vec<String>),

    /// Already negotiated out of band (TLS / WebSocket ALPN). Reported as-is;
    /// the `application_protocols` parameter is never sent, and receiving it is
    /// a protocol error.
    Negotiated(String),
}

/// Configuration for a QMux session.
///
/// Construct with [`Config::new`] (or [`Config::negotiated`]) and set the public
/// fields you need; the struct is `#[non_exhaustive]` so new fields can be added
/// without breaking callers. For the common transports, prefer the higher-level
/// [`tcp`](crate::tcp) / [`uds`](crate::uds) builders, which wrap this.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
    /// Wire format version.
    pub version: Version,
    /// How the application protocol is determined. See [`Protocol`].
    pub protocol: Protocol,

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

    /// Idle timeout in milliseconds (0 = disabled). Only used by the
    /// record-framed drafts (QMux01+).
    pub max_idle_timeout: u64,
    /// Maximum QMux Record size in bytes (draft-01+). Default: 16382.
    pub max_record_size: u64,

    /// Largest DATAGRAM *frame* (RFC 9221: frame type + length + payload) we
    /// advertise willingness to receive, in bytes; `0` disables datagrams. Sent
    /// verbatim as the `max_datagram_frame_size` transport parameter.
    ///
    /// This is a frame size, not a payload size: the usable payload reported by
    /// [`max_datagram_size`](web_transport_trait::Session::max_datagram_size) is
    /// this value less the per-frame overhead. A datagram must fit within a
    /// single record, so keep it at or below
    /// [`max_record_size`](Config::max_record_size). Datagrams are a
    /// record-framed-draft feature (QMux01+); this is ignored on QMux00 and the
    /// legacy `webtransport` format. Default:
    /// [`DEFAULT_MAX_RECORD_SIZE`](crate::proto::DEFAULT_MAX_RECORD_SIZE).
    pub max_datagram_frame_size: u64,

    /// How long [`Session::connect`](crate::Session::connect) /
    /// [`accept`](crate::Session::accept) waits for the peer's transport
    /// parameters before giving up. Bounds the handshake so a peer that completes
    /// the transport connection but never sends its parameters can't hang
    /// establishment forever. Default: 10s; a zero duration disables the timeout
    /// (wait indefinitely).
    pub handshake_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: Version::QMux02,
            protocol: Protocol::None,
            max_streams_bidi: 100,
            max_streams_uni: 100,
            max_data: 1_048_576,                  // 1 MB
            max_stream_data_bidi_local: 262_144,  // 256 KB
            max_stream_data_bidi_remote: 262_144, // 256 KB
            max_stream_data_uni: 262_144,         // 256 KB
            max_idle_timeout: 30_000,             // 30 seconds
            max_record_size: DEFAULT_MAX_RECORD_SIZE,
            // Fill a full record by default; the record layer bounds the size.
            max_datagram_frame_size: DEFAULT_MAX_RECORD_SIZE,
            handshake_timeout: Duration::from_secs(10),
        }
    }
}

impl Config {
    /// Create a config with default flow control values and no application
    /// protocol. Set [`Config::protocol`] to negotiate one.
    pub fn new(version: Version) -> Self {
        Self {
            version,
            ..Default::default()
        }
    }

    /// Create a config whose protocol was already negotiated out of band
    /// (TLS / WebSocket ALPN). `protocol` is the chosen name, or `None`.
    pub fn negotiated(version: Version, protocol: Option<String>) -> Self {
        Self {
            protocol: match protocol {
                Some(name) => Protocol::Negotiated(name),
                None => Protocol::None,
            },
            ..Self::new(version)
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
            // Datagrams rely on the record layer to be framed, so only advertise
            // the parameter on the record-framed drafts (draft-01+). Elsewhere it
            // stays 0 — "unsupported" — which the encoder omits. Clamp to
            // max_record_size so we never invite a datagram larger than our record
            // layer accepts (recv_record would reject it as FrameTooLarge and kill
            // the session).
            max_datagram_frame_size: if self.version.uses_records() {
                self.max_datagram_frame_size.min(self.max_record_size)
            } else {
                0
            },
            max_record_size: self.max_record_size,
            // Only advertise protocols when negotiating in-band; TLS/WS already
            // chose one via ALPN and must not send this parameter.
            protocols: match &self.protocol {
                Protocol::Negotiate(list) => list.clone(),
                Protocol::None | Protocol::Negotiated(_) => Vec::new(),
            },
        }
    }
}
