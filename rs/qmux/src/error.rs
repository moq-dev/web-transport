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

    #[error("connection closed: {code}: {reason}")]
    ConnectionClosed { code: VarInt, reason: String },

    #[error("stream reset: {0}")]
    StreamReset(VarInt),

    #[error("stream stop: {0}")]
    StreamStop(VarInt),

    #[error("frame too large")]
    FrameTooLarge,

    #[error("flow control error")]
    FlowControlError,

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
