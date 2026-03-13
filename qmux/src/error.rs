use web_transport_proto::{VarInt, VarIntUnexpectedEnd};

/// Errors that can occur during QMux session and stream operations.
#[derive(Debug, thiserror::Error, Clone)]
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

    #[error("short frame")]
    Short,

    #[error("connection closed")]
    Closed,

    #[error("invalid protocol token: {0:?}")]
    InvalidProtocol(String),

    #[error("io error: {0}")]
    Io(String),

    #[error("datagrams not supported")]
    DatagramsUnsupported,
}

impl From<VarIntUnexpectedEnd> for Error {
    fn from(_: VarIntUnexpectedEnd) -> Self {
        Self::Short
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

#[cfg(feature = "ws")]
impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::Io(err.to_string())
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
