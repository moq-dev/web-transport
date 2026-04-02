use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

// ---------------------------------------------------------------------------
// Exception hierarchy
// ---------------------------------------------------------------------------

// Base
create_exception!(_web_transport, WebTransportError, PyException);

// -- Session errors ---------------------------------------------------------
create_exception!(_web_transport, SessionError, WebTransportError);
create_exception!(_web_transport, ConnectError, SessionError);
create_exception!(_web_transport, SessionRejected, ConnectError);
create_exception!(_web_transport, SessionClosed, SessionError);
create_exception!(_web_transport, SessionClosedByPeer, SessionClosed);
create_exception!(_web_transport, SessionClosedLocally, SessionClosed);
create_exception!(_web_transport, SessionTimeout, SessionError);
create_exception!(_web_transport, ProtocolError, SessionError);

// -- Stream errors ----------------------------------------------------------
create_exception!(_web_transport, StreamError, WebTransportError);
create_exception!(_web_transport, StreamClosed, StreamError);
create_exception!(_web_transport, StreamClosedByPeer, StreamClosed);
create_exception!(_web_transport, StreamClosedLocally, StreamClosed);
create_exception!(_web_transport, StreamTooLongError, StreamError);
create_exception!(_web_transport, StreamIncompleteReadError, StreamError);

// -- Datagram errors --------------------------------------------------------
create_exception!(_web_transport, DatagramError, WebTransportError);
create_exception!(_web_transport, DatagramTooLargeError, DatagramError);
create_exception!(_web_transport, DatagramNotSupportedError, DatagramError);

// ---------------------------------------------------------------------------
// Exception constructors with custom attributes
// ---------------------------------------------------------------------------

fn set_attrs(err: &PyErr, f: impl FnOnce(&Bound<'_, pyo3::types::PyAny>)) {
    Python::attach(|py| {
        let val = err.value(py);
        f(val.as_any());
    });
}

pub fn new_session_closed_by_peer(source: &str, code: Option<u64>, reason: &str) -> PyErr {
    let code_str = code.map(|c| format!(" (code {c})")).unwrap_or_default();
    let msg = match source {
        "session" => format!("peer closed session{code_str}: {reason}"),
        "application" => format!("peer application closed session{code_str}: {reason}"),
        "transport" => format!("transport closed{code_str}: {reason}"),
        "connection-reset" => "peer sent stateless reset".to_string(),
        _ => format!("session closed by peer{code_str}: {reason}"),
    };
    let err = SessionClosedByPeer::new_err(msg);
    let source = source.to_string();
    let reason = reason.to_string();
    set_attrs(&err, |val| {
        let _ = val.setattr("source", &*source);
        let _ = val.setattr("code", code);
        let _ = val.setattr("reason", &*reason);
    });
    err
}

pub fn new_stream_closed_by_peer(kind: &str, code: u32) -> PyErr {
    let msg = format!("stream {kind} by peer with code {code}");
    let err = StreamClosedByPeer::new_err(msg);
    let kind = kind.to_string();
    set_attrs(&err, |val| {
        let _ = val.setattr("kind", &*kind);
        let _ = val.setattr("code", code);
    });
    err
}

pub fn new_stream_incomplete_read_error(expected: usize, partial: &[u8]) -> PyErr {
    let got = partial.len();
    let msg = format!("expected {expected} bytes, got {got} before EOF");
    let err = StreamIncompleteReadError::new_err(msg);
    let partial = partial.to_vec();
    set_attrs(&err, |val| {
        let _ = val.setattr("expected", expected);
        let _ = val.setattr("partial", &*partial);
    });
    err
}

pub fn new_session_rejected(status_code: u16, msg: String) -> PyErr {
    let err = SessionRejected::new_err(msg);
    set_attrs(&err, |val| {
        let _ = val.setattr("status_code", status_code);
    });
    err
}

pub fn new_datagram_not_supported_error(reason: &str) -> PyErr {
    let msg = format!("datagrams not available: {reason}");
    let err = DatagramNotSupportedError::new_err(msg);
    let reason = reason.to_string();
    set_attrs(&err, |val| {
        let _ = val.setattr("reason", &*reason);
    });
    err
}

// ---------------------------------------------------------------------------
// Error mapping from Rust types to Python exceptions
// ---------------------------------------------------------------------------

/// Map a `quinn::ConnectionError` to a Python exception.
pub fn map_connection_error(err: quinn::ConnectionError) -> PyErr {
    match err {
        quinn::ConnectionError::TimedOut => SessionTimeout::new_err("session timed out"),
        quinn::ConnectionError::LocallyClosed => {
            SessionClosedLocally::new_err("session closed locally")
        }
        quinn::ConnectionError::ApplicationClosed(ref close) => {
            let code = web_transport_quinn::proto::error_from_http3(close.error_code.into_inner())
                .map(|c| c as u64);
            let reason = String::from_utf8_lossy(&close.reason).to_string();
            new_session_closed_by_peer("application", code, &reason)
        }
        quinn::ConnectionError::ConnectionClosed(ref close) => {
            let code: u64 = close.error_code.into();
            let reason = String::from_utf8_lossy(&close.reason).to_string();
            new_session_closed_by_peer("transport", Some(code), &reason)
        }
        quinn::ConnectionError::Reset => new_session_closed_by_peer("connection-reset", None, ""),
        quinn::ConnectionError::TransportError(ref te) => ProtocolError::new_err(te.to_string()),
        quinn::ConnectionError::VersionMismatch => ProtocolError::new_err("QUIC version mismatch"),
        quinn::ConnectionError::CidsExhausted => ProtocolError::new_err("connection IDs exhausted"),
    }
}

/// Map a `web_transport_quinn::SessionError` to a Python exception.
pub fn map_session_error(err: web_transport_quinn::SessionError) -> PyErr {
    match err {
        web_transport_quinn::SessionError::ConnectionError(ce) => map_connection_error(ce),
        web_transport_quinn::SessionError::WebTransportError(ref wte) => match wte {
            web_transport_quinn::WebTransportError::Closed(code, reason) => {
                // WebTransport CLOSE_WEBTRANSPORT_SESSION capsule from the peer
                new_session_closed_by_peer("session", Some(*code as u64), reason)
            }
            _ => ProtocolError::new_err(wte.to_string()),
        },
        web_transport_quinn::SessionError::SendDatagramError(sde) => map_send_datagram_error(sde),
    }
}

/// Map a `web_transport_quinn::WriteError` to a Python exception.
pub fn map_write_error(err: web_transport_quinn::WriteError) -> PyErr {
    match err {
        web_transport_quinn::WriteError::Stopped(code) => new_stream_closed_by_peer("stop", code),
        web_transport_quinn::WriteError::InvalidStopped(_) => {
            ProtocolError::new_err("peer sent STOP_SENDING with invalid error code")
        }
        web_transport_quinn::WriteError::SessionError(se) => map_session_error(se),
        // ClosedStream is only produced when the local side has already called
        // finish() or reset(), moving SendState out of Ready.  Peer actions
        // (STOP_SENDING) produce WriteError::Stopped instead.
        web_transport_quinn::WriteError::ClosedStream => {
            StreamClosedLocally::new_err("stream already closed")
        }
    }
}

/// Map a `web_transport_quinn::ReadError` to a Python exception.
pub fn map_read_error(err: web_transport_quinn::ReadError) -> PyErr {
    match err {
        web_transport_quinn::ReadError::Reset(code) => new_stream_closed_by_peer("reset", code),
        web_transport_quinn::ReadError::InvalidReset(_) => {
            ProtocolError::new_err("peer sent RESET_STREAM with invalid error code")
        }
        web_transport_quinn::ReadError::SessionError(se) => map_session_error(se),
        // ClosedStream is only produced when the local side has already called
        // stop().  Peer actions (RESET_STREAM) produce ReadError::Reset instead.
        web_transport_quinn::ReadError::ClosedStream => {
            StreamClosedLocally::new_err("stream already closed")
        }
        web_transport_quinn::ReadError::IllegalOrderedRead => {
            ProtocolError::new_err("illegal ordered read on unordered stream")
        }
    }
}

/// Map a `web_transport_quinn::ReadToEndError` to a Python exception.
pub fn map_read_to_end_error(err: web_transport_quinn::ReadToEndError, limit: usize) -> PyErr {
    match err {
        web_transport_quinn::ReadToEndError::TooLong => {
            let msg = format!("stream data exceeded {limit} byte limit");
            let err = StreamTooLongError::new_err(msg);
            set_attrs(&err, |val| {
                let _ = val.setattr("limit", limit);
            });
            err
        }
        web_transport_quinn::ReadToEndError::ReadError(re) => map_read_error(re),
    }
}

/// Map a `web_transport_quinn::ReadExactError` to a Python exception.
pub fn map_read_exact_error(
    err: web_transport_quinn::ReadExactError,
    expected: usize,
    buf: &[u8],
) -> PyErr {
    match err {
        web_transport_quinn::ReadExactError::FinishedEarly(bytes_read) => {
            new_stream_incomplete_read_error(expected, &buf[..bytes_read])
        }
        web_transport_quinn::ReadExactError::ReadError(re) => map_read_error(re),
    }
}

/// Map a `quinn::SendDatagramError` to a Python exception.
pub fn map_send_datagram_error(err: quinn::SendDatagramError) -> PyErr {
    match err {
        quinn::SendDatagramError::UnsupportedByPeer => {
            new_datagram_not_supported_error("unsupported_by_peer")
        }
        quinn::SendDatagramError::Disabled => new_datagram_not_supported_error("disabled_locally"),
        quinn::SendDatagramError::TooLarge => DatagramTooLargeError::new_err("datagram too large"),
        quinn::SendDatagramError::ConnectionLost(ce) => map_connection_error(ce),
    }
}

/// Map a `web_transport_quinn::ClientError` to a Python exception.
///
/// This is only called from `Client.connect()`, so every error is
/// mapped to `ConnectError` (or its subclass `SessionRejected`) —
/// the session was never established, and error types like
/// `SessionTimeout` or `SessionClosedByPeer` would be misleading
/// when no session exists yet.
pub fn map_client_error(err: web_transport_quinn::ClientError) -> PyErr {
    match &err {
        // Server rejected the session with a non-200 HTTP status code.
        web_transport_quinn::ClientError::HttpError(
            web_transport_quinn::ConnectError::ProtoError(
                web_transport_quinn::proto::ConnectError::WrongStatus(Some(status)),
            ),
        ) => new_session_rejected(status.as_u16(), err.to_string()),
        _ => ConnectError::new_err(err.to_string()),
    }
}

/// Map a `web_transport_quinn::ServerError` to a Python exception.
pub fn map_server_error(err: web_transport_quinn::ServerError) -> PyErr {
    match err {
        web_transport_quinn::ServerError::Connection(ce) => map_connection_error(ce),
        web_transport_quinn::ServerError::UnexpectedEnd
        | web_transport_quinn::ServerError::WriteError(_)
        | web_transport_quinn::ServerError::ReadError(_)
        | web_transport_quinn::ServerError::SettingsError(_)
        | web_transport_quinn::ServerError::ConnectError(_)
        | web_transport_quinn::ServerError::IoError(_)
        | web_transport_quinn::ServerError::Rustls(_) => SessionError::new_err(err.to_string()),
    }
}

/// Register all exception types on the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("WebTransportError", m.py().get_type::<WebTransportError>())?;
    m.add("SessionError", m.py().get_type::<SessionError>())?;
    m.add("ConnectError", m.py().get_type::<ConnectError>())?;
    m.add("SessionRejected", m.py().get_type::<SessionRejected>())?;
    m.add("SessionClosed", m.py().get_type::<SessionClosed>())?;
    m.add(
        "SessionClosedByPeer",
        m.py().get_type::<SessionClosedByPeer>(),
    )?;
    m.add(
        "SessionClosedLocally",
        m.py().get_type::<SessionClosedLocally>(),
    )?;
    m.add("SessionTimeout", m.py().get_type::<SessionTimeout>())?;
    m.add("ProtocolError", m.py().get_type::<ProtocolError>())?;
    m.add("StreamError", m.py().get_type::<StreamError>())?;
    m.add("StreamClosed", m.py().get_type::<StreamClosed>())?;
    m.add(
        "StreamClosedByPeer",
        m.py().get_type::<StreamClosedByPeer>(),
    )?;
    m.add(
        "StreamClosedLocally",
        m.py().get_type::<StreamClosedLocally>(),
    )?;
    m.add(
        "StreamTooLongError",
        m.py().get_type::<StreamTooLongError>(),
    )?;
    m.add(
        "StreamIncompleteReadError",
        m.py().get_type::<StreamIncompleteReadError>(),
    )?;
    m.add("DatagramError", m.py().get_type::<DatagramError>())?;
    m.add(
        "DatagramTooLargeError",
        m.py().get_type::<DatagramTooLargeError>(),
    )?;
    m.add(
        "DatagramNotSupportedError",
        m.py().get_type::<DatagramNotSupportedError>(),
    )?;
    Ok(())
}
