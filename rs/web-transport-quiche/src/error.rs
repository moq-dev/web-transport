use web_transport_proto::error_from_http3;

use crate::ez;

/// An error returned by [Connection], split based on whether they are underlying QUIC errors or WebTransport errors.
#[derive(Clone, thiserror::Error, Debug)]
pub enum SessionError {
    #[error("remote closed: code={0} reason={1}")]
    Remote(u32, String),

    #[error("local closed: code={0} reason={1}")]
    Local(u32, String),

    #[error("connection error: {0}")]
    Connection(ez::ConnectionError),

    #[error("invalid stream header: {0}")]
    Header(ez::StreamError),

    #[error("unknown session")]
    Unknown,
}

/// An error when reading from or writing to a WebTransport stream.
#[derive(thiserror::Error, Debug)]
pub enum StreamError {
    #[error("session error: {0}")]
    Session(#[from] SessionError),

    #[error("reset stream: {0})")]
    Reset(u32),

    #[error("stop stream: {0})")]
    Stop(u32),

    #[error("invalid reset code: {0}")]
    InvalidReset(u64),

    #[error("invalid reset code: {0}")]
    InvalidStop(u64),

    #[error("stream closed")]
    Closed,
}

impl From<ez::ConnectionError> for SessionError {
    fn from(err: ez::ConnectionError) -> Self {
        match &err {
            ez::ConnectionError::Remote(code, reason) => match error_from_http3(*code) {
                Some(code) => SessionError::Remote(code, reason.clone()),
                None => SessionError::Connection(err),
            },
            ez::ConnectionError::Local(code, reason) => match error_from_http3(*code) {
                Some(code) => SessionError::Local(code, reason.clone()),
                None => SessionError::Connection(err),
            },
            _ => SessionError::Connection(err),
        }
    }
}

impl From<ez::StreamError> for StreamError {
    fn from(err: ez::StreamError) -> Self {
        match err {
            ez::StreamError::Reset(code) => match web_transport_proto::error_from_http3(code) {
                Some(code) => StreamError::Reset(code),
                None => StreamError::InvalidReset(code),
            },
            ez::StreamError::Connection(e) => StreamError::Session(e.into()),
            ez::StreamError::Stop(code) => match web_transport_proto::error_from_http3(code) {
                Some(code) => StreamError::Stop(code),
                None => StreamError::InvalidStop(code),
            },
            ez::StreamError::Closed => StreamError::Closed,
        }
    }
}

impl web_transport_trait::Error for StreamError {
    fn session_error(&self) -> Option<(u32, String)> {
        if let StreamError::Session(e) = self {
            return e.session_error();
        }

        None
    }

    fn stream_error(&self) -> Option<u32> {
        match self {
            StreamError::Reset(code) | StreamError::Stop(code) => Some(*code),
            _ => None,
        }
    }
}
impl web_transport_trait::Error for SessionError {
    fn session_error(&self) -> Option<(u32, String)> {
        match self {
            SessionError::Remote(code, reason) => Some((*code, reason.clone())),
            SessionError::Local(code, reason) => Some((*code, reason.clone())),
            _ => None,
        }
    }
}
