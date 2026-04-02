/// The wire format version used for frame encoding/decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Version {
    /// Legacy "webtransport" wire format (WebSocket only).
    /// Frame type as u8, no flow control, simplified STREAM/RESET_STREAM/CONNECTION_CLOSE.
    WebTransport,
    /// QMux draft-00 wire format (any transport).
    /// VarInt frame types, QUIC v1 frame encoding, transport parameters.
    QMux00,
    /// QMux draft-01 wire format (any transport).
    /// Adds QMux Records framing, QX_PING, PADDING, max_idle_timeout, max_record_size.
    QMux01,
}

impl Version {
    /// Returns true if the version uses QMux framing (draft-00 or later).
    pub fn is_qmux(self) -> bool {
        matches!(self, Version::QMux00 | Version::QMux01)
    }

    /// The bare ALPN identifier for this version (e.g. `"qmux-01"`).
    pub fn alpn(self) -> &'static str {
        match self {
            Version::WebTransport => "webtransport",
            Version::QMux00 => "qmux-00",
            Version::QMux01 => "qmux-01",
        }
    }

    /// The ALPN/subprotocol prefix for this version (e.g. `"qmux-01."`).
    pub fn prefix(self) -> &'static str {
        match self {
            Version::WebTransport => "webtransport.",
            Version::QMux00 => "qmux-00.",
            Version::QMux01 => "qmux-01.",
        }
    }

    /// All supported versions, in preference order (newest first).
    pub const ALL: &[Version] = &[Version::QMux01, Version::QMux00, Version::WebTransport];
}
