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
}
