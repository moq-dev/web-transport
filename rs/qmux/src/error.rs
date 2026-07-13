use std::sync::Arc;

use web_transport_proto::{VarInt, VarIntBoundsExceeded, VarIntUnexpectedEnd};

/// Errors that can occur during QMux session and stream operations.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid frame type: {0}")]
    InvalidFrameType(u64),

    #[error("invalid stream id")]
    InvalidStreamId,

    #[error("stream closed")]
    StreamClosed,

    /// The peer sent an APPLICATION_CLOSE (0x1d): a graceful, deliberate session
    /// close carrying an application code and reason. Surfaced as a clean session
    /// error (see [`session_error`](web_transport_trait::Error::session_error)).
    #[error("connection closed: {code}: {reason}")]
    ConnectionClosed { code: VarInt, reason: String },

    /// The peer sent a CONNECTION_CLOSE (0x1c): it detected a protocol violation
    /// or transport error. Abnormal — unlike [`ConnectionClosed`](Self::ConnectionClosed)
    /// it does *not* surface as a clean application close, mirroring how a QUIC
    /// transport CONNECTION_CLOSE rejects rather than gracefully ending a session.
    #[error("connection reset: {code}: {reason}")]
    ConnectionReset { code: VarInt, reason: String },

    #[error("stream reset: {0}")]
    StreamReset(VarInt),

    #[error("stream stop: {0}")]
    StreamStop(VarInt),

    #[error("frame too large")]
    FrameTooLarge,

    #[error("flow control error")]
    FlowControlError,

    /// A frame was malformed on the wire — QMux's FRAME_ENCODING_ERROR. Draft-02
    /// raises this e.g. when a RESET_STREAM_AT frame's Reliable Size exceeds its
    /// Final Size.
    #[error("frame encoding error")]
    FrameEncoding,

    /// The peer broke a protocol rule — QMux's PROTOCOL_VIOLATION. Draft-02
    /// raises this for QX_TRANSPORT_PARAMETERS sent out of order (or twice) and
    /// for invalid QX_PING sequence numbers.
    #[error("protocol violation")]
    ProtocolViolation,

    /// A transport parameter carried an illegal value — QMux's
    /// TRANSPORT_PARAMETER_ERROR. Draft-02 raises this when a peer advertises a
    /// `max_record_size` below the default minimum.
    #[error("transport parameter error")]
    TransportParameter,

    #[error("stream limit exceeded")]
    StreamLimitExceeded,

    #[error("duplicate transport parameter: 0x{0:02x}")]
    DuplicateParam(u64),

    #[error("short frame")]
    Short,

    #[error("connection closed")]
    Closed,

    #[error("idle timeout")]
    IdleTimeout,

    #[error("handshake timeout: peer never sent transport parameters")]
    HandshakeTimeout,

    #[error("invalid protocol token: {0:?}")]
    InvalidProtocol(String),

    /// Peer sent the `application_protocols` transport parameter, but this
    /// session isn't negotiating in-band (e.g. it's a TLS/WebSocket session that
    /// already negotiated via ALPN, or a stream session that didn't opt in).
    #[error("unexpected application_protocols parameter (in-band negotiation not enabled)")]
    UnexpectedProtocols,

    #[error("invalid server name")]
    InvalidServerName,

    /// The server rejected the handshake with a non-success HTTP status.
    ///
    /// Surfaced from the WebSocket upgrade response so callers can distinguish
    /// terminal failures (e.g. 401/403 auth rejections) from transient I/O
    /// errors worth retrying.
    #[error("http error status: {0}")]
    Http(u16),

    // Foreign error sources aren't `Clone`, but `Error` must be (the session
    // fans one terminal error out to every waiter). `Arc` bridges the two while
    // keeping the original error — source chain and all — instead of a string.
    #[error(transparent)]
    Io(Arc<std::io::Error>),

    #[cfg(feature = "ws")]
    #[error(transparent)]
    WebSocket(Arc<tokio_tungstenite::tungstenite::Error>),

    #[error("datagrams not supported")]
    DatagramsUnsupported,
}

impl Error {
    /// The wire error code to send on a CONNECTION_CLOSE (0x1c) when *we* tear the
    /// session down because of this error, or `None` when the peer should not (or
    /// cannot) be told: a graceful close, a close the peer already sent us, an idle
    /// timeout (silent, like QUIC), or a dead transport. Every protocol/transport
    /// violation we detect maps to 1002, matching the JS QMux polyfill.
    pub(crate) fn transport_close(&self) -> Option<u32> {
        match self {
            Error::ProtocolViolation
            | Error::FrameEncoding
            | Error::TransportParameter
            | Error::DuplicateParam(_)
            | Error::InvalidFrameType(_)
            | Error::InvalidStreamId
            | Error::FlowControlError
            | Error::StreamLimitExceeded
            | Error::FrameTooLarge
            | Error::DatagramsUnsupported
            | Error::Short => Some(1002),
            _ => None,
        }
    }
}

impl From<VarIntUnexpectedEnd> for Error {
    fn from(_: VarIntUnexpectedEnd) -> Self {
        Self::Short
    }
}

impl From<VarIntBoundsExceeded> for Error {
    fn from(_: VarIntBoundsExceeded) -> Self {
        Self::FlowControlError
    }
}

// Hand-written rather than `#[from]`: thiserror's `#[from]` would generate
// `From<Arc<std::io::Error>>`, but `?` call sites yield a bare `std::io::Error`.
// These wrap it so `?` stays ergonomic.
impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(Arc::new(err))
    }
}

#[cfg(feature = "ws")]
impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        // A non-101 upgrade response carries the HTTP status; surface it as a
        // distinct, transport-agnostic variant so callers can act on terminal
        // rejections (e.g. auth) without downcasting tungstenite.
        if let tokio_tungstenite::tungstenite::Error::Http(response) = &err {
            return Self::Http(response.status().as_u16());
        }
        Self::WebSocket(Arc::new(err))
    }
}

impl web_transport_trait::Error for Error {
    fn session_error(&self) -> Option<(u32, String)> {
        match self {
            Error::ConnectionClosed { code, reason } => match code.into_inner().try_into() {
                Ok(code) => Some((code, reason.clone())),
                Err(_) => None,
            },
            _ => None,
        }
    }

    fn stream_error(&self) -> Option<u32> {
        match self {
            Error::StreamReset(code) | Error::StreamStop(code) => code.into_inner().try_into().ok(),
            _ => None,
        }
    }
}

#[cfg(all(test, feature = "ws"))]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::{self, http};

    #[test]
    fn preserves_http_status() {
        let response = http::Response::builder()
            .status(http::StatusCode::UNAUTHORIZED)
            .body(None::<Vec<u8>>)
            .unwrap();
        let err: Error = tungstenite::Error::Http(Box::new(response)).into();
        assert!(matches!(err, Error::Http(401)));
    }

    #[test]
    fn non_http_preserves_source() {
        let err: Error = tungstenite::Error::ConnectionClosed.into();
        assert!(matches!(err, Error::WebSocket(_)));
    }
}
