use web_transport_proto::{VarInt, VarIntUnexpectedEnd};

#[derive(Debug, thiserror::Error, Clone)]
pub enum Error {
    #[error("invalid frame type: {0}")]
    InvalidFrameType(u8),

    #[error("text messages not allowed")]
    NoText,

    #[error("pong messages not allowed")]
    NoPong,

    #[error("generic frames not allowed")]
    NoGenericFrames,

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
}

impl From<VarIntUnexpectedEnd> for Error {
    fn from(_: VarIntUnexpectedEnd) -> Self {
        Self::Short
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for Error {
    fn from(_err: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::Closed
    }
}

impl web_transport_trait::Error for Error {
    fn session_error(&self) -> Option<(u32, String)> {
        match self {
            // TODO We should only support u32 on the wire?
            Error::ConnectionClosed { code, reason } => match code.into_inner().try_into() {
                Ok(code) => Some((code, reason.clone())),
                Err(_) => None,
            },
            _ => None,
        }
    }

    fn stream_error(&self) -> Option<u32> {
        match self {
            // TODO We should only support u32 on the wire?
            Error::StreamReset(code) | Error::StreamStop(code) => code.into_inner().try_into().ok(),
            _ => None,
        }
    }
}
