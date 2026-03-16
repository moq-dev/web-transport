use std::sync::Arc;

use iroh::endpoint;
use n0_error::stack_error;

use crate::{ConnectError, SettingsError};

/// An error returned when connecting to a WebTransport endpoint.
#[stack_error(derive, from_sources)]
#[derive(Clone)]
pub enum ClientError {
    #[error("unexpected end of stream")]
    UnexpectedEnd,

    #[error("failed to connect")]
    Connect(#[error(source)] Arc<endpoint::ConnectError>),

    #[error("connection failed")]
    Connection(#[error(source, std_err)] endpoint::ConnectionError),

    #[error("failed to write")]
    WriteError(#[error(source, std_err)] endpoint::WriteError),

    #[error("failed to read")]
    ReadError(#[error(source, std_err)] endpoint::ReadError),

    #[error("failed to exchange h3 settings")]
    SettingsError(#[error(from, source, std_err)] SettingsError),

    #[error("failed to exchange h3 connect")]
    HttpError(#[error(from, source, std_err)] ConnectError),

    #[error("invalid URL")]
    InvalidUrl,

    #[error("endpoint failed to bind")]
    Bind(#[error(source)] Arc<endpoint::BindError>),
}

/// An error returned by [`crate::Session`], split between underlying QUIC errors and WebTransport errors.
#[stack_error(derive, from_sources)]
#[derive(Clone)]
pub enum SessionError {
    #[error("connection error")]
    ConnectionError(#[error(source, from, std_err)] endpoint::ConnectionError),

    #[error("webtransport error")]
    WebTransportError(#[error(source, from, std_err)] WebTransportError),

    #[error("send datagram error")]
    SendDatagramError(#[error(source, from, std_err)] endpoint::SendDatagramError),
}

/// An error that can occur when reading/writing the WebTransport stream header.
#[stack_error(derive, from_sources)]
#[derive(Clone)]
pub enum WebTransportError {
    #[error("closed: code={code} reason={reason}")]
    Closed { code: u32, reason: String },

    #[error("unknown session")]
    UnknownSession,

    #[error("read error")]
    ReadError(#[error(source, from, std_err)] endpoint::ReadExactError),

    #[error("write error")]
    WriteError(#[error(source, from, std_err)] endpoint::WriteError),
}

/// An error when writing to [`crate::SendStream`]. Similar to [`iroh::endpoint::WriteError`].
#[stack_error(derive, from_sources)]
#[derive(Clone)]
pub enum WriteError {
    #[error("STOP_SENDING: {_0}")]
    Stopped(u32),

    #[error("invalid STOP_SENDING: {_0}")]
    InvalidStopped(endpoint::VarInt),

    #[error("session error")]
    SessionError(#[error(source, from)] SessionError),

    #[error("stream closed")]
    ClosedStream,
}

impl From<endpoint::WriteError> for WriteError {
    fn from(e: endpoint::WriteError) -> Self {
        match e {
            endpoint::WriteError::Stopped(code) => {
                match web_transport_proto::error_from_http3(code.into_inner()) {
                    Some(code) => WriteError::Stopped(code),
                    None => WriteError::InvalidStopped(code),
                }
            }
            endpoint::WriteError::ClosedStream => WriteError::ClosedStream,
            endpoint::WriteError::ConnectionLost(e) => WriteError::SessionError(e.into()),
            endpoint::WriteError::ZeroRttRejected => unreachable!("0-RTT not supported"),
        }
    }
}

/// An error when reading from [`crate::RecvStream`]. Similar to [`iroh::endpoint::ReadError`].
#[stack_error(derive, from_sources)]
#[derive(Clone)]
pub enum ReadError {
    #[error("session error")]
    SessionError(#[error(source, from)] SessionError),

    #[error("RESET_STREAM: {_0}")]
    Reset(u32),

    #[error("invalid RESET_STREAM: {_0}")]
    InvalidReset(endpoint::VarInt),

    #[error("stream already closed")]
    ClosedStream,
}

impl From<endpoint::ReadError> for ReadError {
    fn from(value: endpoint::ReadError) -> Self {
        match value {
            endpoint::ReadError::Reset(code) => {
                match web_transport_proto::error_from_http3(code.into_inner()) {
                    Some(code) => ReadError::Reset(code),
                    None => ReadError::InvalidReset(code),
                }
            }
            endpoint::ReadError::ConnectionLost(e) => Self::SessionError(e.into()),
            endpoint::ReadError::ClosedStream => Self::ClosedStream,
            endpoint::ReadError::ZeroRttRejected => unreachable!("0-RTT not supported"),
        }
    }
}

/// An error returned by [`crate::RecvStream::read_exact`]. Similar to [`iroh::endpoint::ReadExactError`].
#[stack_error(derive, from_sources)]
#[derive(Clone)]
pub enum ReadExactError {
    #[error("finished early")]
    FinishedEarly(usize),

    #[error("read error")]
    ReadError(#[error(source, from)] ReadError),
}

impl From<endpoint::ReadExactError> for ReadExactError {
    fn from(e: endpoint::ReadExactError) -> Self {
        match e {
            endpoint::ReadExactError::FinishedEarly(size) => ReadExactError::FinishedEarly(size),
            endpoint::ReadExactError::ReadError(e) => ReadExactError::ReadError(e.into()),
        }
    }
}

/// An error returned by [`crate::RecvStream::read_to_end`]. Similar to [`iroh::endpoint::ReadToEndError`].
#[stack_error(derive, from_sources)]
#[derive(Clone)]
pub enum ReadToEndError {
    #[error("too long")]
    TooLong,

    #[error("read error")]
    ReadError(#[error(source, from)] ReadError),
}

impl From<endpoint::ReadToEndError> for ReadToEndError {
    fn from(e: endpoint::ReadToEndError) -> Self {
        match e {
            endpoint::ReadToEndError::TooLong => ReadToEndError::TooLong,
            endpoint::ReadToEndError::Read(e) => ReadToEndError::ReadError(e.into()),
        }
    }
}

/// An error indicating the stream was already closed.
#[stack_error(derive)]
#[derive(Clone)]
#[error("stream closed")]
pub struct ClosedStream;

impl From<endpoint::ClosedStream> for ClosedStream {
    fn from(_: endpoint::ClosedStream) -> Self {
        ClosedStream
    }
}

/// An error returned when receiving a new WebTransport session.
#[stack_error(derive, from_sources)]
#[derive(Clone)]
pub enum ServerError {
    #[error("unexpected end of stream")]
    UnexpectedEnd,

    #[error("connection failed")]
    Connection(#[error(source, std_err)] endpoint::ConnectionError),

    #[error("connection failed during handshake")]
    Connecting(#[error(source)] Arc<endpoint::ConnectingError>),

    #[error("failed to write")]
    WriteError(#[error(source, std_err)] endpoint::WriteError),

    #[error("failed to read")]
    ReadError(#[error(source, std_err)] endpoint::ReadError),

    #[error("io error")]
    IoError(#[error(source)] Arc<std::io::Error>),

    #[error("failed to bind endpoint")]
    Bind(#[error(source)] Arc<endpoint::BindError>),

    #[error("failed to exchange h3 connect")]
    HttpError(#[error(source, from, std_err)] ConnectError),

    #[error("failed to exchange h3 settings")]
    SettingsError(#[error(source, from, std_err)] SettingsError),
}

impl web_transport_trait::Error for SessionError {
    fn session_error(&self) -> Option<(u32, String)> {
        if let SessionError::WebTransportError(WebTransportError::Closed { code, reason }) = self {
            return Some((*code, reason.to_string()));
        }

        None
    }
}

impl web_transport_trait::Error for WriteError {
    fn session_error(&self) -> Option<(u32, String)> {
        if let WriteError::SessionError(e) = self {
            return e.session_error();
        }

        None
    }

    fn stream_error(&self) -> Option<u32> {
        match self {
            WriteError::Stopped(code) => Some(*code),
            _ => None,
        }
    }
}

impl web_transport_trait::Error for ReadError {
    fn session_error(&self) -> Option<(u32, String)> {
        if let ReadError::SessionError(e) = self {
            return e.session_error();
        }

        None
    }

    fn stream_error(&self) -> Option<u32> {
        match self {
            ReadError::Reset(code) => Some(*code),
            _ => None,
        }
    }
}
