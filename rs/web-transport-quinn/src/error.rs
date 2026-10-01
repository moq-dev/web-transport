use std::sync::{Arc, OnceLock};

use thiserror::Error;

use crate::{ConnectError, SettingsError};

/// An error returned when connecting to a WebTransport endpoint.
#[derive(Error, Debug, Clone)]
pub enum ClientError {
    #[error("unexpected end of stream")]
    UnexpectedEnd,

    #[error("connection error: {0}")]
    Connection(#[from] quinn::ConnectionError),

    #[error("failed to write: {0}")]
    WriteError(#[from] quinn::WriteError),

    #[error("failed to read: {0}")]
    ReadError(#[from] quinn::ReadError),

    #[error("failed to exchange h3 settings: {0}")]
    SettingsError(#[from] SettingsError),

    #[error("failed to exchange h3 connect: {0}")]
    HttpError(#[from] ConnectError),

    #[error("quic error: {0}")]
    QuinnError(#[from] quinn::ConnectError),

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
    ConnectionError(quinn::ConnectionError),

    #[error("webtransport error: {0}")]
    WebTransportError(#[from] WebTransportError),

    #[error("send datagram error: {0}")]
    SendDatagramError(#[from] quinn::SendDatagramError),
}

impl From<quinn::ConnectionError> for SessionError {
    fn from(e: quinn::ConnectionError) -> Self {
        match &e {
            quinn::ConnectionError::ApplicationClosed(close) => {
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

/// The session's first close reason and its application-code space.
#[derive(Debug)]
pub(crate) struct CloseReason {
    raw: bool,
    reason: OnceLock<SessionError>,
}

impl CloseReason {
    pub(crate) fn new(raw: bool) -> Self {
        Self {
            raw,
            reason: OnceLock::new(),
        }
    }

    pub(crate) fn get(&self) -> Option<&SessionError> {
        self.reason.get()
    }

    pub(crate) fn set(&self, err: SessionError) -> Result<(), SessionError> {
        self.reason.set(err)
    }

    pub(crate) fn map(&self, err: SessionError) -> SessionError {
        let connection = match &err {
            SessionError::ConnectionError(connection)
            | SessionError::SendDatagramError(quinn::SendDatagramError::ConnectionLost(
                connection,
            )) => Some(connection),
            SessionError::WebTransportError(WebTransportError::Closed(..)) => None,
            _ => return err,
        };
        if let Some(reason) = self.get() {
            return reason.clone();
        }
        if self.raw {
            if let Some(quinn::ConnectionError::ApplicationClosed(close)) = connection {
                if let Ok(code) = u32::try_from(close.error_code.into_inner()) {
                    return WebTransportError::Closed(
                        code,
                        String::from_utf8_lossy(&close.reason).into_owned(),
                    )
                    .into();
                }
            }
        }
        err
    }
}

/// An error that can occur when reading/writing the WebTransport stream header.
#[derive(Clone, Error, Debug)]
pub enum WebTransportError {
    #[error("closed: code={0} reason={1}")]
    Closed(u32, String),

    #[error("unknown session")]
    UnknownSession,

    #[error("read error: {0}")]
    ReadError(#[from] quinn::ReadExactError),

    #[error("write error: {0}")]
    WriteError(#[from] quinn::WriteError),
}

/// An error when writing to [`crate::SendStream`]. Similar to [`quinn::WriteError`].
#[derive(Clone, Error, Debug)]
pub enum WriteError {
    #[error("STOP_SENDING: {0}")]
    Stopped(u32),

    #[error("invalid STOP_SENDING: {0}")]
    InvalidStopped(quinn::VarInt),

    #[error("session error: {0}")]
    SessionError(#[from] SessionError),

    #[error("stream closed")]
    ClosedStream,
}

impl From<quinn::WriteError> for WriteError {
    fn from(e: quinn::WriteError) -> Self {
        match e {
            quinn::WriteError::Stopped(code) => {
                match web_transport_proto::error_from_http3(code.into_inner()) {
                    Some(code) => WriteError::Stopped(code),
                    None => WriteError::InvalidStopped(code),
                }
            }
            quinn::WriteError::ClosedStream => WriteError::ClosedStream,
            quinn::WriteError::ConnectionLost(e) => WriteError::SessionError(e.into()),
            quinn::WriteError::ZeroRttRejected => unreachable!("0-RTT not supported"),
        }
    }
}

/// An error when reading from [`crate::RecvStream`]. Similar to [`quinn::ReadError`].
#[derive(Clone, Error, Debug)]
pub enum ReadError {
    #[error("session error: {0}")]
    SessionError(#[from] SessionError),

    #[error("RESET_STREAM: {0}")]
    Reset(u32),

    #[error("invalid RESET_STREAM: {0}")]
    InvalidReset(quinn::VarInt),

    #[error("stream already closed")]
    ClosedStream,

    #[error("ordered read on unordered stream")]
    IllegalOrderedRead,
}

impl From<quinn::ReadError> for ReadError {
    fn from(value: quinn::ReadError) -> Self {
        match value {
            quinn::ReadError::Reset(code) => {
                match web_transport_proto::error_from_http3(code.into_inner()) {
                    Some(code) => ReadError::Reset(code),
                    None => ReadError::InvalidReset(code),
                }
            }
            quinn::ReadError::ConnectionLost(e) => ReadError::SessionError(e.into()),
            quinn::ReadError::IllegalOrderedRead => ReadError::IllegalOrderedRead,
            quinn::ReadError::ClosedStream => ReadError::ClosedStream,
            quinn::ReadError::ZeroRttRejected => unreachable!("0-RTT not supported"),
        }
    }
}

/// An error returned by [`crate::RecvStream::read_exact`]. Similar to [`quinn::ReadExactError`].
#[derive(Clone, Error, Debug)]
pub enum ReadExactError {
    #[error("finished early")]
    FinishedEarly(usize),

    #[error("read error: {0}")]
    ReadError(#[from] ReadError),
}

impl From<quinn::ReadExactError> for ReadExactError {
    fn from(e: quinn::ReadExactError) -> Self {
        match e {
            quinn::ReadExactError::FinishedEarly(size) => ReadExactError::FinishedEarly(size),
            quinn::ReadExactError::ReadError(e) => ReadExactError::ReadError(e.into()),
        }
    }
}

/// An error returned by [`crate::RecvStream::read_to_end`]. Similar to [`quinn::ReadToEndError`].
#[derive(Clone, Error, Debug)]
pub enum ReadToEndError {
    #[error("too long")]
    TooLong,

    #[error("read error: {0}")]
    ReadError(#[from] ReadError),
}

impl From<quinn::ReadToEndError> for ReadToEndError {
    fn from(e: quinn::ReadToEndError) -> Self {
        match e {
            quinn::ReadToEndError::TooLong => ReadToEndError::TooLong,
            quinn::ReadToEndError::Read(e) => ReadToEndError::ReadError(e.into()),
        }
    }
}

/// An error indicating the stream was already closed.
#[derive(Clone, Error, Debug)]
#[error("stream closed")]
pub struct ClosedStream;

impl From<quinn::ClosedStream> for ClosedStream {
    fn from(_: quinn::ClosedStream) -> Self {
        ClosedStream
    }
}

/// An error returned when receiving a new WebTransport session.
#[derive(Error, Debug, Clone)]
pub enum ServerError {
    #[error("unexpected end of stream")]
    UnexpectedEnd,

    #[error("connection error")]
    Connection(#[from] quinn::ConnectionError),

    #[error("failed to write")]
    WriteError(#[from] quinn::WriteError),

    #[error("failed to read")]
    ReadError(#[from] quinn::ReadError),

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

// impl From<quinn::SendDatagramError> for SendDatagramError {
//     fn from(value: quinn::SendDatagramError) -> Self {
//          match value {
//              quinn::SendDatagramError::UnsupportedByPeer => SendDatagramError::UnsupportedPeer,
//              quinn::SendDatagramError::Disabled => SendDatagramError::DatagramSupportDisabled,
//              quinn::SendDatagramError::TooLarge => SendDatagramError::TooLarge,
//              quinn::SendDatagramError::ConnectionLost(e) => SendDatagramError::SessionError(e.into()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use web_transport_trait::Error as _;

    fn peer_close(code: u64) -> SessionError {
        quinn::ConnectionError::ApplicationClosed(quinn::ApplicationClose {
            error_code: quinn::VarInt::from_u64(code).unwrap(),
            reason: b"peer closed".as_slice().into(),
        })
        .into()
    }

    #[test]
    fn raw_close_uses_the_application_code_space() {
        let raw = CloseReason::new(true);
        assert_eq!(
            raw.map(peer_close(4075)).session_error(),
            Some((4075, "peer closed".into()))
        );
        assert_eq!(
            raw.map(peer_close(u32::MAX as u64 + 1)).session_error(),
            None
        );
    }

    #[test]
    fn http3_close_keeps_its_code_space() {
        let h3 = CloseReason::new(false);
        assert_eq!(h3.map(peer_close(4075)).session_error(), None);
        assert_eq!(
            h3.map(peer_close(web_transport_proto::error_to_http3(4075)))
                .session_error(),
            Some((4075, "peer closed".into()))
        );
    }
}
