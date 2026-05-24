//! Error enum exported to UniFFI.
//!
//! The legacy PyO3 binding raised a class hierarchy
//! (`SessionClosedByPeer`, `StreamClosedLocally`, etc.); the Python wrapper
//! in `py/web-transport/python/web_transport/_errors.py` reconstructs that
//! hierarchy from this enum's variants. Kotlin and Swift see the variants
//! directly.
//!
//! Not `#[uniffi(flat_error)]` — we need structured field access on the
//! foreign side (e.g. `SessionClosedByPeer.code`, `StreamIncompleteRead.partial`).

/// Error returned by all UniFFI-exported functions.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum WebTransportError {
    // ---- session errors -------------------------------------------------
    #[error("connect: {0}")]
    Connect(String),

    #[error("session rejected: HTTP {status_code}: {detail}")]
    SessionRejected { status_code: u16, detail: String },

    #[error("session closed by peer ({closed_by}): {reason}")]
    SessionClosedByPeer {
        closed_by: String,
        code: Option<u64>,
        reason: String,
    },

    #[error("session closed locally")]
    SessionClosedLocally,

    #[error("session timed out")]
    SessionTimeout,

    #[error("protocol: {0}")]
    Protocol(String),

    // ---- stream errors --------------------------------------------------
    #[error("stream {kind} by peer with code {code}")]
    StreamClosedByPeer { kind: String, code: u32 },

    #[error("stream closed locally")]
    StreamClosedLocally,

    #[error("stream data exceeded {limit} byte limit")]
    StreamTooLong { limit: u64 },

    #[error("expected {expected} bytes, got {got} before EOF")]
    StreamIncompleteRead {
        expected: u64,
        got: u64,
        partial: Vec<u8>,
    },

    // ---- datagram errors ------------------------------------------------
    #[error("datagram too large")]
    DatagramTooLarge,

    #[error("datagrams not supported: {reason}")]
    DatagramNotSupported { reason: String },

    // ---- miscellaneous --------------------------------------------------
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("io: {0}")]
    Io(String),

    #[error("cancelled")]
    Cancelled,
}

impl WebTransportError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidArgument(msg.into())
    }

    pub fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
    }
}

// ---------------------------------------------------------------------------
// Mapping from web-transport-quinn / quinn types to WebTransportError.
// Mirrors `rs/web-transport-python/src/errors.rs`.
// ---------------------------------------------------------------------------

fn close_reason_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn map_connection_error(err: quinn::ConnectionError) -> WebTransportError {
    match err {
        quinn::ConnectionError::TimedOut => WebTransportError::SessionTimeout,
        quinn::ConnectionError::LocallyClosed => WebTransportError::SessionClosedLocally,
        quinn::ConnectionError::ApplicationClosed(ref close) => {
            let code = web_transport_quinn::proto::error_from_http3(close.error_code.into_inner())
                .map(|c| c as u64);
            WebTransportError::SessionClosedByPeer {
                closed_by: "application".into(),
                code,
                reason: close_reason_string(&close.reason),
            }
        }
        quinn::ConnectionError::ConnectionClosed(ref close) => {
            let code: u64 = close.error_code.into();
            WebTransportError::SessionClosedByPeer {
                closed_by: "transport".into(),
                code: Some(code),
                reason: close_reason_string(&close.reason),
            }
        }
        quinn::ConnectionError::Reset => WebTransportError::SessionClosedByPeer {
            closed_by: "connection-reset".into(),
            code: None,
            reason: String::new(),
        },
        quinn::ConnectionError::TransportError(ref te) => {
            WebTransportError::protocol(te.to_string())
        }
        quinn::ConnectionError::VersionMismatch => {
            WebTransportError::protocol("QUIC version mismatch")
        }
        quinn::ConnectionError::CidsExhausted => {
            WebTransportError::protocol("connection IDs exhausted")
        }
    }
}

pub fn map_session_error(err: web_transport_quinn::SessionError) -> WebTransportError {
    match err {
        web_transport_quinn::SessionError::ConnectionError(ce) => map_connection_error(ce),
        web_transport_quinn::SessionError::WebTransportError(ref wte) => match wte {
            web_transport_quinn::WebTransportError::Closed(code, reason) => {
                WebTransportError::SessionClosedByPeer {
                    closed_by: "session".into(),
                    code: Some(*code as u64),
                    reason: reason.clone(),
                }
            }
            _ => WebTransportError::protocol(wte.to_string()),
        },
        web_transport_quinn::SessionError::SendDatagramError(sde) => map_send_datagram_error(sde),
    }
}

pub fn map_write_error(err: web_transport_quinn::WriteError) -> WebTransportError {
    match err {
        web_transport_quinn::WriteError::Stopped(code) => WebTransportError::StreamClosedByPeer {
            kind: "stop".into(),
            code,
        },
        web_transport_quinn::WriteError::InvalidStopped(_) => {
            WebTransportError::protocol("peer sent STOP_SENDING with invalid error code")
        }
        web_transport_quinn::WriteError::SessionError(se) => map_session_error(se),
        web_transport_quinn::WriteError::ClosedStream => WebTransportError::StreamClosedLocally,
    }
}

pub fn map_read_error(err: web_transport_quinn::ReadError) -> WebTransportError {
    match err {
        web_transport_quinn::ReadError::Reset(code) => WebTransportError::StreamClosedByPeer {
            kind: "reset".into(),
            code,
        },
        web_transport_quinn::ReadError::InvalidReset(_) => {
            WebTransportError::protocol("peer sent RESET_STREAM with invalid error code")
        }
        web_transport_quinn::ReadError::SessionError(se) => map_session_error(se),
        web_transport_quinn::ReadError::ClosedStream => WebTransportError::StreamClosedLocally,
        web_transport_quinn::ReadError::IllegalOrderedRead => {
            WebTransportError::protocol("illegal ordered read on unordered stream")
        }
    }
}

pub fn map_read_to_end_error(
    err: web_transport_quinn::ReadToEndError,
    limit: usize,
) -> WebTransportError {
    match err {
        web_transport_quinn::ReadToEndError::TooLong => WebTransportError::StreamTooLong {
            limit: limit as u64,
        },
        web_transport_quinn::ReadToEndError::ReadError(re) => map_read_error(re),
    }
}

pub fn map_read_exact_error(
    err: web_transport_quinn::ReadExactError,
    expected: usize,
    buf: &[u8],
) -> WebTransportError {
    match err {
        web_transport_quinn::ReadExactError::FinishedEarly(bytes_read) => {
            WebTransportError::StreamIncompleteRead {
                expected: expected as u64,
                got: bytes_read as u64,
                partial: buf[..bytes_read].to_vec(),
            }
        }
        web_transport_quinn::ReadExactError::ReadError(re) => map_read_error(re),
    }
}

pub fn map_send_datagram_error(err: quinn::SendDatagramError) -> WebTransportError {
    match err {
        quinn::SendDatagramError::UnsupportedByPeer => WebTransportError::DatagramNotSupported {
            reason: "unsupported_by_peer".into(),
        },
        quinn::SendDatagramError::Disabled => WebTransportError::DatagramNotSupported {
            reason: "disabled_locally".into(),
        },
        quinn::SendDatagramError::TooLarge => WebTransportError::DatagramTooLarge,
        quinn::SendDatagramError::ConnectionLost(ce) => map_connection_error(ce),
    }
}

pub fn map_client_error(err: web_transport_quinn::ClientError) -> WebTransportError {
    match &err {
        web_transport_quinn::ClientError::HttpError(
            web_transport_quinn::ConnectError::ProtoError(
                web_transport_quinn::proto::ConnectError::WrongStatus(Some(status)),
            ),
        ) => WebTransportError::SessionRejected {
            status_code: status.as_u16(),
            detail: err.to_string(),
        },
        _ => WebTransportError::Connect(err.to_string()),
    }
}

pub fn map_server_error(err: web_transport_quinn::ServerError) -> WebTransportError {
    match err {
        web_transport_quinn::ServerError::Connection(ce) => map_connection_error(ce),
        web_transport_quinn::ServerError::UnexpectedEnd
        | web_transport_quinn::ServerError::WriteError(_)
        | web_transport_quinn::ServerError::ReadError(_)
        | web_transport_quinn::ServerError::SettingsError(_)
        | web_transport_quinn::ServerError::ConnectError(_)
        | web_transport_quinn::ServerError::IoError(_)
        | web_transport_quinn::ServerError::Rustls(_) => {
            WebTransportError::protocol(err.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_maps_to_session_timeout() {
        let mapped = map_connection_error(quinn::ConnectionError::TimedOut);
        assert!(matches!(mapped, WebTransportError::SessionTimeout));
    }

    #[test]
    fn locally_closed_maps_to_session_closed_locally() {
        let mapped = map_connection_error(quinn::ConnectionError::LocallyClosed);
        assert!(matches!(mapped, WebTransportError::SessionClosedLocally));
    }

    #[test]
    fn datagram_too_large_maps_to_datagram_too_large() {
        let mapped = map_send_datagram_error(quinn::SendDatagramError::TooLarge);
        assert!(matches!(mapped, WebTransportError::DatagramTooLarge));
    }

    #[test]
    fn version_mismatch_maps_to_protocol() {
        let mapped = map_connection_error(quinn::ConnectionError::VersionMismatch);
        assert!(matches!(mapped, WebTransportError::Protocol(_)));
    }
}
