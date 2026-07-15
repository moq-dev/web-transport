/// The wire format version used for frame encoding/decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
#[non_exhaustive]
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
    /// QMux draft-02 wire format (any transport).
    /// Wire-compatible with draft-01, adds the RESET_STREAM_AT frame and the
    /// stricter QX_TRANSPORT_PARAMETERS / QX_PING / max_record_size rules.
    QMux02,
}

impl Version {
    /// Returns true if the version uses QMux framing (draft-00 or later).
    pub fn is_qmux(self) -> bool {
        matches!(self, Version::QMux00 | Version::QMux01 | Version::QMux02)
    }

    /// Returns true if the version uses the QMux Record framing layer and the
    /// features introduced alongside it — the size-prefixed record on byte
    /// streams, QX_PING keep-alive, PADDING, datagrams, `max_idle_timeout`, and
    /// `max_record_size`. True for draft-01 and later; false for draft-00 and the
    /// legacy `webtransport` format, which speak bare frames.
    pub fn uses_records(self) -> bool {
        matches!(self, Version::QMux01 | Version::QMux02)
    }

    /// The bare ALPN identifier for this version (e.g. `"qmux-01"`).
    pub fn alpn(self) -> &'static str {
        match self {
            Version::WebTransport => "webtransport",
            Version::QMux00 => "qmux-00",
            Version::QMux01 => "qmux-01",
            Version::QMux02 => "qmux-02",
        }
    }

    /// The ALPN/subprotocol prefix for this version (e.g. `"qmux-01."`).
    pub fn prefix(self) -> &'static str {
        match self {
            Version::WebTransport => "webtransport.",
            Version::QMux00 => "qmux-00.",
            Version::QMux01 => "qmux-01.",
            Version::QMux02 => "qmux-02.",
        }
    }

    /// All supported versions, in preference order (newest first).
    pub const ALL: &[Version] = &[
        Version::QMux02,
        Version::QMux01,
        Version::QMux00,
        Version::WebTransport,
    ];
}
