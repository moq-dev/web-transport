use std::sync::Arc;

use thiserror::Error;

use crate::{ConnectError, SettingsError};

/// An error returned when connecting to a WebTransport endpoint.
#[derive(Error, Debug, Clone)]
pub enum ClientError {
    #[error("unexpected end of stream")]
    UnexpectedEnd,

    #[error("connection error: {0}")]
    Connection(#[from] noq::ConnectionError),

    #[error("failed to write: {0}")]
    WriteError(#[from] noq::WriteError),

    #[error("failed to read: {0}")]
    ReadError(#[from] noq::ReadError),

    #[error("failed to exchange h3 settings: {0}")]
    SettingsError(#[from] SettingsError),

    #[error("failed to exchange h3 connect: {0}")]
    HttpError(#[from] ConnectError),

    #[error("quic error: {0}")]
    NoqError(#[from] noq::ConnectError),

    #[error("invalid DNS name: {0}")]
    InvalidDnsName(String),

    #[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
    #[error("rustls error: {0}")]
    Rustls(#[from] rustls::Error),
}

/// An errors returned by [`crate::Session`], split based on if they are underlying QUIC errors or WebTransport errors.
#[derive(Clone, Error, Debug)]
pub enum SessionError {
    #[error("connection error: {0}")]
    ConnectionError(noq::ConnectionError),

    #[error("webtransport error: {0}")]
    WebTransportError(#[from] WebTransportError),

    #[error("send datagram error: {0}")]
    SendDatagramError(#[from] noq::SendDatagramError),
}

impl From<noq::ConnectionError> for SessionError {
    fn from(e: noq::ConnectionError) -> Self {
        match &e {
            noq::ConnectionError::ApplicationClosed(close) => {
                match web_transport_proto::error_from_http3(close.error_code.into_inner()) {
                    Some(code) => WebTransportError::Closed(
                        code,
                        String::from_utf8_lossy(&close.reason).into_owned(),
                    )
                    .into(),
                    None => SessionError::ConnectionError(e),
                }
            }
            _ => SessionError::ConnectionError(e),
        }
    }
}

/// The QUIC code to reset or stop a stream with. Raw QUIC sends the code as is, while
/// HTTP/3 maps it into its own code space.
pub(crate) fn encode_stream_code(code: u32, raw: bool) -> noq::VarInt {
    if raw {
        return code.into();
    }
    noq::VarInt::try_from(web_transport_proto::error_to_http3(code)).unwrap()
}

/// Decode the peer's QUIC code for a stream reset or stop, or None if it is not one.
pub(crate) fn decode_stream_code(code: noq::VarInt, raw: bool) -> Option<u32> {
    let code = code.into_inner();
    // An older raw peer maps its codes into the HTTP/3 range too. No u32 reaches that
    // range, so the mapped form cannot be mistaken for a raw code.
    match web_transport_proto::error_from_http3(code) {
        Some(code) => Some(code),
        None if raw => u32::try_from(code).ok(),
        None => None,
    }
}

/// An error in a WebTransport stream header or session capsule.
#[derive(Clone, Error, Debug)]
pub enum WebTransportError {
    #[error("closed: code={0} reason={1}")]
    Closed(u32, String),

    #[error("unknown session")]
    UnknownSession,

    #[error("read error: {0}")]
    ReadError(#[from] noq::ReadExactError),

    #[error("write error: {0}")]
    WriteError(#[from] noq::WriteError),

    /// The session's HTTP/3 capsule stream was malformed or interrupted, with a diagnostic message.
    #[error("capsule error: {0}")]
    CapsuleError(String),
}

/// An error when writing to [`crate::SendStream`]. Similar to [`noq::WriteError`].
#[derive(Clone, Error, Debug)]
pub enum WriteError {
    #[error("STOP_SENDING: {0}")]
    Stopped(u32),

    #[error("invalid STOP_SENDING: {0}")]
    InvalidStopped(noq::VarInt),

    #[error("session error: {0}")]
    SessionError(#[from] SessionError),

    #[error("stream closed")]
    ClosedStream,
}

impl From<noq::WriteError> for WriteError {
    fn from(e: noq::WriteError) -> Self {
        match e {
            noq::WriteError::Stopped(code) => {
                match web_transport_proto::error_from_http3(code.into_inner()) {
                    Some(code) => WriteError::Stopped(code),
                    None => WriteError::InvalidStopped(code),
                }
            }
            noq::WriteError::ClosedStream => WriteError::ClosedStream,
            noq::WriteError::ConnectionLost(e) => WriteError::SessionError(e.into()),
            noq::WriteError::ZeroRttRejected => unreachable!("0-RTT not supported"),
        }
    }
}

/// An error when reading from [`crate::RecvStream`]. Similar to [`noq::ReadError`].
#[derive(Clone, Error, Debug)]
pub enum ReadError {
    #[error("session error: {0}")]
    SessionError(#[from] SessionError),

    #[error("RESET_STREAM: {0}")]
    Reset(u32),

    #[error("invalid RESET_STREAM: {0}")]
    InvalidReset(noq::VarInt),

    #[error("stream already closed")]
    ClosedStream,
}

impl From<noq::ReadError> for ReadError {
    fn from(value: noq::ReadError) -> Self {
        match value {
            noq::ReadError::Reset(code) => {
                match web_transport_proto::error_from_http3(code.into_inner()) {
                    Some(code) => ReadError::Reset(code),
                    None => ReadError::InvalidReset(code),
                }
            }
            noq::ReadError::ConnectionLost(e) => ReadError::SessionError(e.into()),
            noq::ReadError::ClosedStream => ReadError::ClosedStream,
            noq::ReadError::ZeroRttRejected => unreachable!("0-RTT not supported"),
        }
    }
}

/// An error returned by [`crate::RecvStream::read_exact`]. Similar to [`noq::ReadExactError`].
#[derive(Clone, Error, Debug)]
pub enum ReadExactError {
    #[error("finished early")]
    FinishedEarly(usize),

    #[error("read error: {0}")]
    ReadError(#[from] ReadError),
}

impl From<noq::ReadExactError> for ReadExactError {
    fn from(e: noq::ReadExactError) -> Self {
        match e {
            noq::ReadExactError::FinishedEarly(size) => ReadExactError::FinishedEarly(size),
            noq::ReadExactError::ReadError(e) => ReadExactError::ReadError(e.into()),
        }
    }
}

/// An error returned by [`crate::RecvStream::read_to_end`]. Similar to [`noq::ReadToEndError`].
#[derive(Clone, Error, Debug)]
pub enum ReadToEndError {
    #[error("too long")]
    TooLong,

    #[error("read error: {0}")]
    ReadError(#[from] ReadError),
}

impl From<noq::ReadToEndError> for ReadToEndError {
    fn from(e: noq::ReadToEndError) -> Self {
        match e {
            noq::ReadToEndError::TooLong => ReadToEndError::TooLong,
            noq::ReadToEndError::Read(e) => ReadToEndError::ReadError(e.into()),
        }
    }
}

/// An error indicating the stream was already closed.
#[derive(Clone, Error, Debug)]
#[error("stream closed")]
pub struct ClosedStream;

impl From<noq::ClosedStream> for ClosedStream {
    fn from(_: noq::ClosedStream) -> Self {
        ClosedStream
    }
}

/// An error returned when receiving a new WebTransport session.
#[derive(Error, Debug, Clone)]
pub enum ServerError {
    #[error("unexpected end of stream")]
    UnexpectedEnd,

    #[error("connection error")]
    Connection(#[from] noq::ConnectionError),

    #[error("failed to write")]
    WriteError(#[from] noq::WriteError),

    #[error("failed to read")]
    ReadError(#[from] noq::ReadError),

    #[error("failed to exchange h3 settings")]
    SettingsError(#[from] SettingsError),

    #[error("failed to exchange h3 connect")]
    ConnectError(#[from] ConnectError),

    #[error("io error: {0}")]
    IoError(Arc<std::io::Error>),

    #[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
    #[error("rustls error: {0}")]
    Rustls(#[from] rustls::Error),
}

// #[derive(Clone, Error, Debug)]
// pub enum SendDatagramError {
//     #[error("Unsupported peer")]
//     UnsupportedPeer,

//     #[error("Datagram support Disabled by peer")]
//     DatagramSupportDisabled,

//     #[error("Datagram Too large")]
//     TooLarge,

//     #[error("Session errorr: {0}")]
//     SessionError(#[from] SessionError),
// }

// impl From<noq::SendDatagramError> for SendDatagramError {
//     fn from(value: noq::SendDatagramError) -> Self {
//          match value {
//              noq::SendDatagramError::UnsupportedByPeer => SendDatagramError::UnsupportedPeer,
//              noq::SendDatagramError::Disabled => SendDatagramError::DatagramSupportDisabled,
//              noq::SendDatagramError::TooLarge => SendDatagramError::TooLarge,
//              noq::SendDatagramError::ConnectionLost(e) => SendDatagramError::SessionError(e.into()),
//          }
//     }
// }

impl web_transport_trait::Error for SessionError {
    fn session_error(&self) -> Option<(u32, String)> {
        if let SessionError::WebTransportError(WebTransportError::Closed(code, reason)) = self {
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
