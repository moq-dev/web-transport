use std::{
    fmt,
    future::Future,
    io::Cursor,
    ops::Deref,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    task::{Context, Poll, Waker},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use futures::stream::{FuturesUnordered, Stream, StreamExt};
use kio::{Fan, Waiter};

use crate::{
    proto::{ConnectRequest, ConnectResponse, Frame, StreamUni, VarInt},
    ClientError, Connected, RecvStream, SendStream, SessionError, Settings, WebTransportError,
};

/// The ALPN the QUIC handshake negotiated, or `None` if there was none, the handshake
/// has not completed, or it is not valid UTF-8.
///
/// Noq only exposes the ALPN via a downcast of the boxed handshake data, so the result
/// is owned rather than borrowed from the connection.
#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
fn negotiated_alpn(conn: &noq::Connection) -> Option<String> {
    let data = conn.handshake_data()?;
    let data = data.downcast::<noq::crypto::rustls::HandshakeData>().ok()?;

    String::from_utf8(data.protocol?).ok()
}

/// `noq::crypto::rustls` only exists once a TLS backend is enabled, so there is no
/// handshake data type to name here.
///
/// This crate's own builders are gated the same way, so normally there is no connection
/// to read an ALPN from either. A caller that enables a provider on `noq` directly and
/// brings its own endpoint can still establish one, and [`Session::protocol`] reports
/// `None` for those raw sessions. Enable this crate's `aws-lc-rs` or `ring` feature to
/// get the ALPN.
#[cfg(not(any(feature = "aws-lc-rs", feature = "ring")))]
fn negotiated_alpn(_conn: &noq::Connection) -> Option<String> {
    None
}

/// An established WebTransport session, acting like a full QUIC connection. See [`noq::Connection`].
///
/// It is important to remember that WebTransport is layered on top of QUIC:
///   1. Each stream starts with a few bytes identifying the stream type and session ID.
///   2. Errors codes are encoded with the session ID, so they aren't full QUIC error codes.
///   3. Stream IDs may have gaps in them, used by HTTP/3 transparent to the application.
///
/// Clones share ownership of the session. Dropping the final [`Session`] closes the
/// connection immediately, even if streams or native connection handles remain.
/// An explicit [`close`](Self::close) already in progress keeps its bounded graceful
/// close instead. Cancelling that task closes locally and cancels the capsule reader,
/// including during runtime shutdown. This ownership contract also applies to [`Session::raw`].
///
/// Deref is used to expose non-overloaded methods on [`noq::Connection`].
/// These should be safe to use with WebTransport, but file a PR if you find one that isn't.
#[derive(Clone)]
pub struct Session {
    conn: noq::Connection,

    // Only frontend Session clones own this guard; internal tasks and streams do not.
    _owner: Arc<SessionOwner>,

    // The session ID, as determined by the stream ID of the connect request.
    session_id: Option<VarInt>,

    // The accept logic is stateful, so use an Arc<Mutex> to share it.
    accept: Option<Arc<Mutex<SessionAccept>>>,

    // Cache the headers in front of each stream we open.
    header_uni: Vec<u8>,
    header_bi: Vec<u8>,
    header_datagram: Vec<u8>,

    // Keep HTTP/3 control streams alive until the session or its graceful close ends.
    settings: Option<Arc<Settings>>,

    // The send side of the CONNECT stream, used to write the CloseWebTransportSession capsule.
    // Wrapped in Arc<Mutex<Option<...>>> so close() can take it exactly once.
    connect_send: Arc<Mutex<Option<noq::SendStream>>>,

    // Session error, set once by either local close() or the background task
    // when a remote CloseWebTransportSession capsule is received.
    // Uses OnceLock for set-once, first-writer-wins semantics with lock-free reads.
    error: Arc<OnceLock<SessionError>>,

    // The request sent by the client, or None for a raw QUIC session.
    request: Option<ConnectRequest>,

    // The response sent by the server, or None for a raw QUIC session.
    response: Option<ConnectResponse>,

    // The ALPN negotiated by the QUIC handshake, for raw QUIC sessions. Noq only
    // exposes it behind an allocating downcast, so `protocol()` has nothing in the
    // connection to borrow from and needs somewhere to keep the result.
    //
    // Resolved on first read rather than at construction: `raw()` accepts any
    // connection, including one whose handshake has not completed and so has no ALPN
    // yet. Only a successful read is latched, so an early call cannot pin `None` for
    // the life of the session. Shared across clones.
    //
    // Unused by HTTP/3 sessions, which read the subprotocol out of the response.
    alpn: Arc<OnceLock<String>>,
}

struct SessionOwner {
    conn: noq::Connection,
    error: Arc<OnceLock<SessionError>>,
    reader: Option<tokio::task::AbortHandle>,
    drop_code: noq::VarInt,
}

/// Own teardown independently of the graceful future being polled or completing.
struct CloseCleanup {
    conn: noq::Connection,
    reader: Option<tokio::task::AbortHandle>,
    code: noq::VarInt,
    reason: Bytes,
}

impl Drop for CloseCleanup {
    fn drop(&mut self) {
        // Noq's close replaces its stored error, so preserve observed peer errors.
        if self.conn.close_reason().is_none() {
            self.conn.close(self.code, &self.reason);
        }
        if let Some(reader) = &self.reader {
            reader.abort();
        }
    }
}

impl Drop for SessionOwner {
    fn drop(&mut self) {
        if self.conn.close_reason().is_none() {
            // A pending explicit close owns teardown and keeps the reader running
            // until its bounded capsule exchange finishes.
            let error = noq::ConnectionError::LocallyClosed.into();
            if self.error.set(error).is_err() {
                return;
            }
            self.conn.close(self.drop_code, b"");
        }

        if let Some(reader) = &self.reader {
            reader.abort();
        }
    }
}

impl Session {
    pub(crate) fn new(conn: noq::Connection, settings: Settings, connect: Connected) -> Self {
        // The session ID is the stream ID of the CONNECT request.
        let session_id = connect.session_id();

        // Cache the tiny header we write in front of each stream we open.
        let mut header_uni = Vec::new();
        StreamUni::WEBTRANSPORT.encode(&mut header_uni);
        session_id.encode(&mut header_uni);

        let mut header_bi = Vec::new();
        Frame::WEBTRANSPORT.encode(&mut header_bi);
        session_id.encode(&mut header_bi);

        let mut header_datagram = Vec::new();
        session_id.encode(&mut header_datagram);

        let error: Arc<OnceLock<SessionError>> = Arc::new(OnceLock::new());

        // Accept logic is stateful, so use an Arc<Mutex> to share it.
        let accept = SessionAccept::new(conn.clone(), session_id, error.clone());

        // The reader owns native handles only, so it cannot keep the frontend alive.
        let reader = tokio::spawn(Self::run_recv(conn.clone(), connect.recv, error.clone()));
        let owner = Arc::new(SessionOwner {
            conn: conn.clone(),
            error: error.clone(),
            reader: Some(reader.abort_handle()),
            drop_code: web_transport_proto::error_to_http3(0).try_into().unwrap(),
        });

        Self {
            conn,
            _owner: owner,
            accept: Some(Arc::new(Mutex::new(accept))),
            session_id: Some(session_id),
            header_uni,
            header_bi,
            header_datagram,
            settings: Some(Arc::new(settings)),
            connect_send: Arc::new(Mutex::new(Some(connect.send))),
            error: error.clone(),
            request: Some(connect.request.clone()),
            response: Some(connect.response.clone()),
            alpn: Default::default(),
        }
    }

    // Read capsules from the CONNECT recv stream until it's closed,
    // then record the close error and tear down the connection.
    async fn run_recv(
        conn: noq::Connection,
        recv: noq::RecvStream,
        error: Arc<OnceLock<SessionError>>,
    ) {
        let close_info = Self::read_capsules(recv).await;
        let code = close_info.as_ref().map_or(0, |(c, _)| *c);

        let http3_code: noq::VarInt = web_transport_proto::error_to_http3(code)
            .try_into()
            .unwrap();

        // Try to record the remote close error. If close() already set
        // the error, it owns the connection teardown, so we bail out.
        match close_info {
            Some((code, reason)) => {
                let err = WebTransportError::Closed(code, reason.clone());
                if error.set(err.into()).is_err() {
                    return;
                }
                conn.close(http3_code, reason.as_bytes());
            }
            None => {
                let err = noq::ConnectionError::LocallyClosed.into();
                if error.set(err).is_err() {
                    return;
                }
                conn.close(http3_code, b"");
            }
        };
    }

    // Keep reading capsules from the CONNECT recv stream until it's closed.
    // Returns Some((code, reason)) if a CloseWebTransportSession capsule was received,
    // or None if the stream closed without a capsule.
    async fn read_capsules(recv: noq::RecvStream) -> Option<(u32, String)> {
        let mut reader = web_transport_proto::Http3CapsuleReader::new(recv);
        loop {
            match reader.read().await {
                Ok(Some(web_transport_proto::Capsule::CloseWebTransportSession {
                    code,
                    reason,
                })) => return Some((code, reason)),
                Ok(Some(web_transport_proto::Capsule::Grease { .. })) => {}
                Ok(Some(web_transport_proto::Capsule::Unknown { typ, payload })) => {
                    tracing::warn!(%typ, size = payload.len(), "unknown capsule");
                }
                Ok(None) => return None,
                Err(e) => {
                    tracing::warn!(?e, "failed to read capsule");
                    return None;
                }
            }
        }
    }

    /// Connect using an established QUIC connection if you want to create the connection yourself.
    /// This will only work with a brand new QUIC connection using the HTTP/3 ALPN.
    pub async fn connect(
        conn: noq::Connection,
        request: impl Into<ConnectRequest>,
    ) -> Result<Session, ClientError> {
        let request = request.into();

        // Perform the H3 handshake by sending/receiving SETTINGS frames.
        let settings = Settings::connect(&conn).await?;

        // Send the HTTP/3 CONNECT request.
        let connect = Connected::open(&conn, request).await?;

        // Return the resulting session with a reference to the control/connect streams.
        // If either stream is closed, then the session will be closed, so we need to keep them around.
        let session = Session::new(conn, settings, connect);

        Ok(session)
    }

    /// Accept a new unidirectional stream. See [`noq::Connection::accept_uni`].
    pub async fn accept_uni(&self) -> Result<RecvStream, SessionError> {
        if let Some(accept) = &self.accept {
            // `kio::wait` owns the waiter, so dropping this future — a `timeout` that
            // expires, say — also drops its registration in `SessionAccept`.
            kio::wait(|waiter| poll_accept_uni_shared(accept, waiter))
                .await
                .map_err(|e| self.map_error(e))
        } else {
            let recv = self
                .conn
                .accept_uni()
                .await
                .map_err(|e| self.map_error(e))?;
            Ok(RecvStream::new(recv, self.error.clone()))
        }
    }

    /// Accept a new bidirectional stream. See [`noq::Connection::accept_bi`].
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        if let Some(accept) = &self.accept {
            kio::wait(|waiter| poll_accept_bi_shared(accept, waiter))
                .await
                .map_err(|e| self.map_error(e))
        } else {
            let (send, recv) = self.conn.accept_bi().await.map_err(|e| self.map_error(e))?;
            Ok((
                SendStream::new(send, self.error.clone()),
                RecvStream::new(recv, self.error.clone()),
            ))
        }
    }

    /// Open a new unidirectional stream. See [`noq::Connection::open_uni`].
    pub async fn open_uni(&self) -> Result<SendStream, SessionError> {
        let mut send = self.conn.open_uni().await.map_err(|e| self.map_error(e))?;

        // Set the stream priority to max and then write the stream header.
        // Otherwise the application could write data with lower priority than the header, resulting in queuing.
        // Also the header is very important for determining the session ID without reliable reset.
        send.set_priority(i32::MAX).ok();
        Self::write_full(&mut send, &self.header_uni)
            .await
            .map_err(|e| self.map_error(e))?;

        // Reset the stream priority back to the default of 0.
        send.set_priority(0).ok();
        Ok(SendStream::new(send, self.error.clone()))
    }

    /// Open a new bidirectional stream. See [`noq::Connection::open_bi`].
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        let (mut send, recv) = self.conn.open_bi().await.map_err(|e| self.map_error(e))?;

        // Set the stream priority to max and then write the stream header.
        // Otherwise the application could write data with lower priority than the header, resulting in queuing.
        // Also the header is very important for determining the session ID without reliable reset.
        send.set_priority(i32::MAX).ok();
        Self::write_full(&mut send, &self.header_bi)
            .await
            .map_err(|e| self.map_error(e))?;

        // Reset the stream priority back to the default of 0.
        send.set_priority(0).ok();
        Ok((
            SendStream::new(send, self.error.clone()),
            RecvStream::new(recv, self.error.clone()),
        ))
    }

    /// Asynchronously receives an application datagram from the remote peer.
    ///
    /// This method is used to receive an application datagram sent by the remote
    /// peer over the connection.
    /// It waits for a datagram to become available and returns the received bytes.
    pub async fn read_datagram(&self) -> Result<Bytes, SessionError> {
        let mut datagram = self
            .conn
            .read_datagram()
            .await
            .map_err(|e| self.map_error(e))?;

        let mut cursor = Cursor::new(&datagram);

        if let Some(session_id) = self.session_id {
            // We have to check and strip the session ID from the datagram.
            let actual_id =
                VarInt::decode(&mut cursor).map_err(|_| WebTransportError::UnknownSession)?;
            if actual_id != session_id {
                return Err(WebTransportError::UnknownSession.into());
            }
        }

        // Return the datagram without the session ID.
        let datagram = datagram.split_off(cursor.position() as usize);

        Ok(datagram)
    }

    /// Sends an application datagram to the remote peer.
    ///
    /// Datagrams are unreliable and may be dropped or delivered out of order.
    /// The data must be smaller than [`max_datagram_size`](Self::max_datagram_size).
    pub fn send_datagram(&self, data: Bytes) -> Result<(), SessionError> {
        let result = if !self.header_datagram.is_empty() {
            // Unfortunately, we need to allocate/copy each datagram because of the Noq API.
            // Pls go +1 if you care: https://github.com/quinn-rs/quinn/issues/1724
            let mut buf = BytesMut::with_capacity(self.header_datagram.len() + data.len());

            // Prepend the datagram with the header indicating the session ID.
            buf.extend_from_slice(&self.header_datagram);
            buf.extend_from_slice(&data);

            self.conn.send_datagram(buf.into())
        } else {
            self.conn.send_datagram(data)
        };

        result.map_err(|e| self.map_error(e))?;
        Ok(())
    }

    /// Sends an application datagram to the remote peer, waiting for buffer space if needed.
    ///
    /// Datagrams are unreliable and may be dropped or delivered out of order.
    /// The data must be smaller than [`max_datagram_size`](Self::max_datagram_size).
    pub async fn send_datagram_wait(&self, data: Bytes) -> Result<(), SessionError> {
        let result = if !self.header_datagram.is_empty() {
            // Unfortunately, we need to allocate/copy each datagram because of the Noq API.
            // Pls go +1 if you care: https://github.com/quinn-rs/quinn/issues/1724
            let mut buf = BytesMut::with_capacity(self.header_datagram.len() + data.len());

            // Prepend the datagram with the header indicating the session ID.
            buf.extend_from_slice(&self.header_datagram);
            buf.extend_from_slice(&data);

            self.conn.send_datagram_wait(buf.into()).await
        } else {
            self.conn.send_datagram_wait(data).await
        };

        result.map_err(|e| self.map_error(e))?;
        Ok(())
    }

    /// Computes the maximum size of datagrams that may be passed to
    /// [`send_datagram`](Self::send_datagram), or 0 when the peer does not accept datagrams.
    pub fn max_datagram_size(&self) -> usize {
        self.conn
            .max_datagram_size()
            .unwrap_or(0)
            .saturating_sub(self.header_datagram.len())
    }

    /// Returns the available buffer space for sending datagrams.
    ///
    /// This value is adjusted to account for the WebTransport session header.
    pub fn datagram_send_buffer_space(&self) -> usize {
        self.conn
            .datagram_send_buffer_space()
            .saturating_sub(self.header_datagram.len())
    }

    /// Close the session with an error code and reason.
    ///
    /// When there is a session ID (WebTransport over HTTP/3), a `CloseWebTransportSession`
    /// capsule is written on the CONNECT stream before the QUIC connection is closed.
    /// This allows browser clients to receive the close code and reason via `WebTransport.closed`.
    ///
    /// The capsule write and connection close happen asynchronously in a spawned task.
    /// The entire operation is bounded by three round trips (at least 100 ms),
    /// including writes blocked by flow control. Dropping the final Session preserves
    /// this pending close. Await [`Session::closed()`] to wait for connection teardown;
    /// delivery of the capsule is best effort. The first close code and reason win.
    ///
    /// Cancelling the task, even before its first poll, closes the native connection
    /// and requests waiter wakeups synchronously, then cancels the capsule reader.
    /// The capsule carries the full UTF-8 reason; the native fallback carries at most
    /// 16 bytes at a character boundary. Already observed native errors are preserved.
    /// Runtime shutdown guarantees local cleanup when the task is dropped; peer
    /// delivery, draining and execution of awakened tasks require a live runtime.
    /// Forgotten futures, non-yielding tasks and process abort are outside this guarantee.
    pub fn close(&self, code: u32, reason: &[u8]) {
        // Record the local close error. First writer wins — if the background
        // task already set a remote close error, or close() was already called,
        // this is a no-op.
        let err = SessionError::ConnectionError(noq::ConnectionError::LocallyClosed);
        if self.error.set(err).is_err() {
            return;
        }

        if self.session_id.is_some() {
            // Take the send stream for the capsule write.
            let send = self.connect_send.lock().unwrap().take();

            if let Some(send) = send {
                let reason = String::from_utf8_lossy(reason).into_owned();
                let conn = self.conn.clone();
                let rtt = self
                    .conn
                    .rtt(noq::PathId::ZERO)
                    .unwrap_or(Duration::from_millis(100));
                let timeout = (rtt * 3).max(Duration::from_millis(100));

                // Keep HTTP/3 critical streams alive during the capsule exchange,
                // without retaining frontend Session ownership.
                let settings = self.settings.clone();
                let accept = self.accept.clone();
                // Construct cleanup before constructing/spawning the task: the
                // runtime may drop it without ever polling its async body.
                let close = Self::close_with_capsule(
                    conn,
                    send,
                    code,
                    reason,
                    timeout,
                    self._owner.reader.clone(),
                );
                tokio::spawn(async move {
                    close.await;
                    drop((settings, accept));
                });
            }
        } else {
            // Raw QUIC mode: no capsule needed.
            self.conn.close(code.into(), reason);
        }
    }

    /// Write the CloseWebTransportSession capsule, finish the stream, wait for
    /// the peer to close the connection (or timeout), then force-close.
    fn close_with_capsule(
        conn: noq::Connection,
        mut send: noq::SendStream,
        code: u32,
        reason: String,
        timeout: std::time::Duration,
        reader: Option<tokio::task::AbortHandle>,
    ) -> impl Future<Output = ()> {
        // Noq writes ACKs first, then encodes close only with >25 bytes left.
        // Its truncation undercounts our eight-byte HTTP/3 code, so fit even
        // that minimum budget: type (1) + code (8) + length (1) + reason (16).
        // The capsule still carries the complete UTF-8 reason.
        let mut limit = reason.len().min(16);
        while !reason.is_char_boundary(limit) {
            limit -= 1;
        }
        let cleanup = CloseCleanup {
            conn,
            reader,
            code: web_transport_proto::error_to_http3(code)
                .try_into()
                .unwrap(),
            reason: Bytes::copy_from_slice(&reason.as_bytes()[..limit]),
        };
        let capsule = web_transport_proto::Capsule::CloseWebTransportSession { code, reason };

        async move {
            // The deadline includes flow control, not just the peer-close wait.
            let close = async {
                let mut capsule_bytes = Vec::new();
                capsule.encode(&mut capsule_bytes);

                let mut frame = Vec::new();
                Frame::DATA.encode(&mut frame);
                let Ok(len) = VarInt::try_from(capsule_bytes.len()) else {
                    tracing::warn!("capsule too large to encode as DATA frame");
                    return;
                };
                len.encode(&mut frame);
                frame.extend_from_slice(&capsule_bytes);

                if let Err(e) = send.write_all(&frame).await {
                    tracing::warn!(?e, "failed to write CloseWebTransportSession capsule");
                    return;
                }
                if let Err(e) = send.finish() {
                    tracing::warn!(?e, "failed to finish CONNECT send stream");
                    return;
                }

                cleanup.conn.closed().await;
            };

            if tokio::time::timeout(timeout, close).await.is_err() {
                tracing::debug!("graceful close timed out");
            }
            drop(cleanup);
        }
    }

    /// Wait until the session is closed, returning the error. See [`noq::Connection::closed`].
    ///
    /// If the peer sent a `CloseWebTransportSession` capsule, the returned error will be
    /// [`WebTransportError::Closed`] with the code and reason from the capsule.
    ///
    /// Unlike [`noq::Connection::closed`], this does **not** return early when
    /// [`close()`](Self::close) has been called. It waits for the underlying QUIC
    /// connection to shut down after the best-effort capsule write or its deadline.
    /// Use [`close_reason()`](Self::close_reason) for a non-blocking check.
    pub async fn closed(&self) -> SessionError {
        self.map_error(self.conn.closed().await)
    }

    /// Return why the session was closed, or None if it's not closed. See [`noq::Connection::close_reason`].
    pub fn close_reason(&self) -> Option<SessionError> {
        self.conn.close_reason().map(|e| self.map_error(e))
    }

    /// Replace connection-level errors with the stored session error if available.
    fn map_error(&self, e: impl Into<SessionError>) -> SessionError {
        let e = e.into();
        if let Some(err) = self.error.get() {
            if matches!(
                &e,
                SessionError::ConnectionError(_)
                    | SessionError::SendDatagramError(noq::SendDatagramError::ConnectionLost(_))
            ) {
                return err.clone();
            }
        }
        e
    }

    async fn write_full(send: &mut noq::SendStream, buf: &[u8]) -> Result<(), SessionError> {
        match send.write_all(buf).await {
            Ok(_) => Ok(()),
            Err(noq::WriteError::ConnectionLost(err)) => Err(err.into()),
            Err(err) => Err(WebTransportError::WriteError(err).into()),
        }
    }

    /// Create a new session from a raw QUIC connection.
    ///
    /// This is used to pretend like a QUIC connection is a WebTransport session,
    /// making it easier to support WebTransport and raw QUIC simultaneously.
    ///
    /// The final Session drop closes the connection even when native connection
    /// handles or streams remain. Keep a Session clone to keep the session alive.
    ///
    /// There is no HTTP/3 exchange, so [`Self::request`] and [`Self::response`] both
    /// return `None`. [`Self::protocol`] reports the ALPN negotiated by the QUIC
    /// handshake, read from `conn` on demand — a connection that is still handshaking
    /// (from `into_0rtt`, say) is fine, it just has no ALPN to report until it is done.
    pub fn raw(conn: noq::Connection) -> Self {
        let error = Arc::new(OnceLock::new());
        let owner = Arc::new(SessionOwner {
            conn: conn.clone(),
            error: error.clone(),
            reader: None,
            drop_code: 0u32.into(),
        });
        Self {
            conn,
            _owner: owner,
            session_id: None,
            header_uni: Default::default(),
            header_bi: Default::default(),
            header_datagram: Default::default(),
            accept: None,
            settings: None,
            connect_send: Arc::new(Mutex::new(None)),
            error,
            request: None,
            response: None,
            alpn: Default::default(),
        }
    }

    /// Returns the [`ConnectRequest`] if this session was established over HTTP/3,
    /// or `None` for a raw QUIC session.
    pub fn request(&self) -> Option<&ConnectRequest> {
        self.request.as_ref()
    }

    /// Returns the [`ConnectResponse`] if this session was established over HTTP/3,
    /// or `None` for a raw QUIC session.
    pub fn response(&self) -> Option<&ConnectResponse> {
        self.response.as_ref()
    }

    /// Returns the application protocol negotiated for this session.
    ///
    /// For an HTTP/3 session this is the subprotocol the server selected via
    /// `WT-Available-Protocols`; for a raw QUIC session it is the negotiated ALPN.
    /// `None` if neither was negotiated, the ALPN is not valid UTF-8, or the raw
    /// connection is still handshaking.
    pub fn protocol(&self) -> Option<&str> {
        if let Some(response) = &self.response {
            return response.protocol.as_deref();
        }

        if let Some(alpn) = self.alpn.get() {
            return Some(alpn);
        }

        // Latch only a successful read, so a call made while the connection is still
        // handshaking cannot pin `None` for the life of the session.
        let alpn = negotiated_alpn(&self.conn)?;
        Some(self.alpn.get_or_init(|| alpn))
    }

    /// Return connection-level statistics.
    pub fn stats(&self) -> SessionStats {
        let path = self.conn.path_stats(noq::PathId::ZERO);
        let rtt = self
            .conn
            .rtt(noq::PathId::ZERO)
            .or_else(|| path.map(|p| p.rtt));
        SessionStats {
            stats: self.conn.stats(),
            path,
            rtt,
        }
    }
}

impl Deref for Session {
    type Target = noq::Connection;

    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.conn.fmt(f)
    }
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.conn.stable_id() == other.conn.stable_id()
    }
}

impl Eq for Session {}

// Poll the shared accept state, then wake the *other* accepters once the lock is
// released.
//
// `SessionAccept` does not wake them itself: a waker is free to resume its task
// inline, and the first thing a resumed accepter does is take this same lock. An
// arrival or a failure is exactly what the others are parked waiting to retry
// after, so `Ready` is the signal.
fn poll_accept_uni_shared(
    accept: &Mutex<SessionAccept>,
    waiter: &Waiter,
) -> Poll<Result<RecvStream, SessionError>> {
    let (result, waiters, hold) = {
        let mut accept = accept.lock().unwrap();
        let waiters = accept.uni_waiters.clone();

        // The poll below drives the shared accept futures with this list's waker, and one
        // of them may wake it inline. `Fan` holds those back while this guard is alive.
        let hold = waiters.hold();
        let result = accept.poll_accept_uni(waiter);

        (result, waiters, hold)
    };

    // Dropped after the accept lock, never before: that is where a held-back wake is
    // delivered, and delivering it under the lock is the hazard the hold exists for.
    drop(hold);

    if result.is_ready() {
        waiters.wake();
    }

    result
}

fn poll_accept_bi_shared(
    accept: &Mutex<SessionAccept>,
    waiter: &Waiter,
) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
    let (result, waiters, hold) = {
        let mut accept = accept.lock().unwrap();
        let waiters = accept.bi_waiters.clone();

        // The poll below drives the shared accept futures with this list's waker, and one
        // of them may wake it inline. `Fan` holds those back while this guard is alive.
        let hold = waiters.hold();
        let result = accept.poll_accept_bi(waiter);

        (result, waiters, hold)
    };

    // Dropped after the accept lock, never before: that is where a held-back wake is
    // delivered, and delivering it under the lock is the hazard the hold exists for.
    drop(hold);

    if result.is_ready() {
        waiters.wake();
    }

    result
}

// Type aliases just so clippy doesn't complain about the complexity.
type AcceptUni = dyn Stream<Item = Result<noq::RecvStream, noq::ConnectionError>> + Send;
type AcceptBi =
    dyn Stream<Item = Result<(noq::SendStream, noq::RecvStream), noq::ConnectionError>> + Send;
type PendingUni = dyn Future<Output = Result<(StreamUni, noq::RecvStream), SessionError>> + Send;
type PendingBi =
    dyn Future<Output = Result<Option<(noq::SendStream, noq::RecvStream)>, SessionError>> + Send;

// Logic just for accepting streams, which is annoying because of the stream header.
pub struct SessionAccept {
    session_id: VarInt,

    // Shared session error for propagation to accepted streams.
    error: Arc<OnceLock<SessionError>>,

    // We also need to keep a reference to the qpack streams if the endpoint (incorrectly) creates them.
    // Again, this is just so they don't get closed until we drop the session.
    qpack_encoder: Option<noq::RecvStream>,
    qpack_decoder: Option<noq::RecvStream>,

    accept_uni: Pin<Box<AcceptUni>>,
    accept_bi: Pin<Box<AcceptBi>>,

    // Keep track of work being done to read/write the WebTransport stream header.
    pending_uni: FuturesUnordered<Pin<Box<PendingUni>>>,
    pending_bi: FuturesUnordered<Pin<Box<PendingBi>>>,

    // Waiters from concurrent callers of accept_bi / accept_uni.
    // Every clone of the session polls this one struct, so an arrival has to be fanned
    // out: each caller registers here and all of them are woken when a stream lands —
    // by the caller that saw it, once it has released the lock on this struct.
    //
    bi_waiters: Fan,
    uni_waiters: Fan,

    // `Waker::from(waiters.clone())`, cached so the inner accept futures are polled with
    // the same waker every time. That waker outlives every caller, so an accepter that
    // drops its future cannot take the wakeup path with it, and the layer below holds
    // one registration rather than one per caller.
    bi_waker: Waker,
    uni_waker: Waker,
}

impl SessionAccept {
    pub(crate) fn new(
        conn: noq::Connection,
        session_id: VarInt,
        error: Arc<OnceLock<SessionError>>,
    ) -> Self {
        // Create a stream that just outputs new streams, so it's easy to call from poll.
        let accept_uni = Box::pin(futures::stream::unfold(conn.clone(), |conn| async {
            Some((conn.accept_uni().await, conn))
        }));

        let accept_bi = Box::pin(futures::stream::unfold(conn, |conn| async {
            Some((conn.accept_bi().await, conn))
        }));

        let bi_waiters = Fan::new();
        let uni_waiters = Fan::new();
        let bi_waker = bi_waiters.waker();
        let uni_waker = uni_waiters.waker();

        Self {
            session_id,
            error,

            qpack_decoder: None,
            qpack_encoder: None,

            accept_uni,
            accept_bi,

            pending_uni: FuturesUnordered::new(),
            pending_bi: FuturesUnordered::new(),

            bi_waiters,
            uni_waiters,
            bi_waker,
            uni_waker,
        }
    }

    /// Poll for the next unidirectional WebTransport stream.
    ///
    /// `waiter` is parked until a stream arrives, the accept fails, or the caller drops
    /// it. The registration is weak and owned by the caller: keep the [`Waiter`] alive
    /// until it is woken, or it will be reclaimed and nothing will wake you. Drive this
    /// with [`kio::wait`], which holds the waiter inside the future it builds.
    ///
    /// A `Ready` here means every *other* parked accepter should be woken so it can
    /// retry. This does not do that itself — see `poll_accept_uni_shared`, which wakes
    /// them once the lock on this struct is released.
    //
    // Poll-based because we accept and decode streams in parallel. In async land this
    // would be a `tokio::JoinSet`, but that needs a runtime; `FuturesUnordered` is
    // runtime-agnostic.
    pub fn poll_accept_uni(&mut self, waiter: &Waiter) -> Poll<Result<RecvStream, SessionError>> {
        // Register before polling, not on the way out: the shared waker can fire from the
        // layer below at any point here, and a wake that lands before the caller is on
        // the list would be lost.
        self.uni_waiters.register(waiter);

        let waker = self.uni_waker.clone();
        let cx = &mut Context::from_waker(&waker);

        loop {
            // Accept any new streams.
            if let Poll::Ready(Some(res)) = self.accept_uni.poll_next_unpin(cx) {
                // Start decoding the header and add the future to the list of pending streams.
                let recv = match res {
                    Ok(recv) => recv,
                    Err(e) => {
                        return Poll::Ready(Err(e.into()));
                    }
                };
                let pending = Self::decode_uni(recv, self.session_id);
                self.pending_uni.push(Box::pin(pending));

                continue;
            }

            // Poll the list of pending streams.
            let (typ, recv) = match self.pending_uni.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(res))) => res,
                Poll::Ready(Some(Err(err))) => {
                    log_header_error(err, "unidirectional");
                    continue;
                }
                Poll::Ready(None) | Poll::Pending => return Poll::Pending,
            };

            // Decide if we keep looping based on the type.
            match typ {
                StreamUni::WEBTRANSPORT => {
                    let recv = RecvStream::new(recv, self.error.clone());
                    return Poll::Ready(Ok(recv));
                }
                StreamUni::QPACK_DECODER => {
                    self.qpack_decoder = Some(recv);
                }
                StreamUni::QPACK_ENCODER => {
                    self.qpack_encoder = Some(recv);
                }
                _ => {
                    // ignore unknown streams
                    tracing::debug!(?typ, "ignoring unknown unidirectional stream");
                }
            }
        }
    }

    // Reads the stream header, returning the stream type.
    async fn decode_uni(
        mut recv: noq::RecvStream,
        expected_session: VarInt,
    ) -> Result<(StreamUni, noq::RecvStream), SessionError> {
        // Read the VarInt at the start of the stream.
        let typ = StreamUni(read_varint(&mut recv).await?);

        if typ == StreamUni::WEBTRANSPORT {
            // Read the session_id and validate it
            let session_id = read_varint(&mut recv).await?;
            if session_id != expected_session {
                return Err(WebTransportError::UnknownSession.into());
            }
        }

        // We need to keep a reference to the qpack streams if the endpoint (incorrectly) creates them, so return everything.
        Ok((typ, recv))
    }

    /// Poll for the next bidirectional WebTransport stream.
    ///
    /// The same contract as [`poll_accept_uni`](Self::poll_accept_uni): the `waiter`
    /// registration is weak and owned by the caller, and a `Ready` is what the other
    /// parked accepters need to be woken for.
    pub fn poll_accept_bi(
        &mut self,
        waiter: &Waiter,
    ) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
        // Register before polling; see `poll_accept_uni`.
        self.bi_waiters.register(waiter);

        let waker = self.bi_waker.clone();
        let cx = &mut Context::from_waker(&waker);

        loop {
            // Accept any new streams.
            if let Poll::Ready(Some(res)) = self.accept_bi.poll_next_unpin(cx) {
                // Start decoding the header and add the future to the list of pending streams.
                let (send, recv) = match res {
                    Ok(pair) => pair,
                    Err(e) => {
                        return Poll::Ready(Err(e.into()));
                    }
                };
                let pending = Self::decode_bi(send, recv, self.session_id);
                self.pending_bi.push(Box::pin(pending));

                continue;
            }

            // Poll the list of pending streams.
            let res = match self.pending_bi.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(res))) => res,
                Poll::Ready(Some(Err(err))) => {
                    log_header_error(err, "bidirectional");
                    continue;
                }
                Poll::Ready(None) | Poll::Pending => return Poll::Pending,
            };

            if let Some((send, recv)) = res {
                // Wrap the streams in our own types for correct error codes.
                let send = SendStream::new(send, self.error.clone());
                let recv = RecvStream::new(recv, self.error.clone());
                return Poll::Ready(Ok((send, recv)));
            }

            // Keep looping if it's a stream we want to ignore.
        }
    }

    // Reads the stream header, returning Some if it's a WebTransport stream.
    async fn decode_bi(
        send: noq::SendStream,
        mut recv: noq::RecvStream,
        expected_session: VarInt,
    ) -> Result<Option<(noq::SendStream, noq::RecvStream)>, SessionError> {
        let typ = read_varint(&mut recv).await?;
        if Frame(typ) != Frame::WEBTRANSPORT {
            tracing::debug!(?typ, "ignoring unknown bidirectional stream");
            return Ok(None);
        }

        // Read the session ID and validate it.
        let session_id = read_varint(&mut recv).await?;
        if session_id != expected_session {
            return Err(WebTransportError::UnknownSession.into());
        }

        Ok(Some((send, recv)))
    }
}

// Read a stream header VarInt, keeping the read's real cause. `VarInt::read` reports any
// failure as a truncation, which would hide a stream the peer reset before its header
// arrived.
async fn read_varint(recv: &mut noq::RecvStream) -> Result<VarInt, WebTransportError> {
    let mut buf = [0u8; 8];
    recv.read_exact(&mut buf[..1]).await?;

    // The first two bits encode the length.
    let size = 1 << (buf[0] >> 6);
    recv.read_exact(&mut buf[1..size]).await?;

    Ok(VarInt::decode(&mut &buf[..size]).expect("a complete varint"))
}

// Without reliable reset, a stream the peer resets early loses its header with it, which
// is routine for some applications. Anything else is a misbehaving peer.
fn log_header_error(err: SessionError, direction: &'static str) {
    match err {
        SessionError::WebTransportError(WebTransportError::ReadError(
            noq::ReadExactError::ReadError(
                noq::ReadError::Reset(_) | noq::ReadError::ConnectionLost(_),
            ),
        )) => tracing::debug!(?err, direction, "stream closed before its header"),
        _ => tracing::warn!(?err, direction, "failed to decode stream header"),
    }
}

pub struct SessionStats {
    stats: noq::ConnectionStats,
    path: Option<noq::PathStats>,
    rtt: Option<std::time::Duration>,
}

impl web_transport_trait::Stats for SessionStats {
    fn bytes_sent(&self) -> Option<u64> {
        Some(self.stats.udp_tx.bytes)
    }

    fn bytes_received(&self) -> Option<u64> {
        Some(self.stats.udp_rx.bytes)
    }

    fn bytes_lost(&self) -> Option<u64> {
        self.path.map(|path| path.lost_bytes)
    }

    fn packets_sent(&self) -> Option<u64> {
        Some(self.stats.udp_tx.datagrams)
    }

    fn packets_received(&self) -> Option<u64> {
        Some(self.stats.udp_rx.datagrams)
    }

    fn packets_lost(&self) -> Option<u64> {
        self.path.map(|path| path.lost_packets)
    }

    fn rtt(&self) -> Option<std::time::Duration> {
        self.rtt
    }

    fn estimated_send_rate(&self) -> Option<u64> {
        let path = self.path?;
        let rtt = self.rtt?;
        let rtt_secs = rtt.as_secs_f64();
        if path.cwnd > 0 && rtt_secs > 0.0 {
            Some((path.cwnd as f64 * 8.0 / rtt_secs) as u64)
        } else {
            None
        }
    }
}

impl web_transport_trait::Session for Session {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = SessionError;

    async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
        Self::accept_uni(self).await
    }

    async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
        Self::accept_bi(self).await
    }

    async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
        Self::open_bi(self).await
    }

    async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
        Self::open_uni(self).await
    }

    fn close(&self, code: u32, reason: &str) {
        Self::close(self, code, reason.as_bytes());
    }

    async fn closed(&self) -> Self::Error {
        Self::closed(self).await
    }

    fn send_datagram(&self, data: Bytes) -> Result<(), Self::Error> {
        Self::send_datagram(self, data)
    }

    async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
        Self::read_datagram(self).await
    }

    fn max_datagram_size(&self) -> usize {
        Self::max_datagram_size(self)
    }

    fn protocol(&self) -> Option<&str> {
        Self::protocol(self)
    }

    #[allow(refining_impl_trait)]
    fn stats(&self) -> SessionStats {
        Self::stats(self)
    }
}

#[cfg(all(test, any(feature = "aws-lc-rs", feature = "ring")))]
mod tests {
    use std::{
        net::Ipv4Addr,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        task::Wake,
    };

    use rcgen::{CertifiedKey, KeyPair};
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

    use super::*;

    const SESSION: VarInt = VarInt::from_u32(0);

    /// A connected client and server QUIC connection, plus the endpoints that drive them.
    async fn pair() -> (noq::Connection, noq::Connection, [noq::Endpoint; 2]) {
        pair_with(Default::default()).await
    }

    /// [`pair`], with the client using `transport`.
    async fn pair_with(
        transport: noq::TransportConfig,
    ) -> (noq::Connection, noq::Connection, [noq::Endpoint; 2]) {
        pair_with_transports(transport, Default::default()).await
    }

    async fn pair_with_transports(
        transport: noq::TransportConfig,
        peer_transport: noq::TransportConfig,
    ) -> (noq::Connection, noq::Connection, [noq::Endpoint; 2]) {
        let endpoints = endpoints_with_transports(transport, peer_transport);
        let (client, server) = connect_endpoints(&endpoints).await;
        (client, server, endpoints)
    }

    fn endpoints_with_transports(
        transport: noq::TransportConfig,
        peer_transport: noq::TransportConfig,
    ) -> [noq::Endpoint; 2] {
        #[cfg(all(feature = "aws-lc-rs", feature = "ring"))]
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(
            &signing_key,
        )));

        let mut server_config =
            noq::ServerConfig::with_single_cert(vec![cert.der().clone()], key).unwrap();
        server_config.transport_config(Arc::new(peer_transport));
        let server = noq::Endpoint::server(server_config, (Ipv4Addr::LOCALHOST, 0).into()).unwrap();

        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.der().clone()).unwrap();
        let mut client_config = noq::ClientConfig::with_root_certificates(Arc::new(roots)).unwrap();
        client_config.transport_config(Arc::new(transport));
        let client = noq::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        client.set_default_client_config(client_config);
        [client, server]
    }

    async fn connect_endpoints(
        endpoints: &[noq::Endpoint; 2],
    ) -> (noq::Connection, noq::Connection) {
        let [client, server] = endpoints;
        let connecting = client
            .connect(server.local_addr().unwrap(), "localhost")
            .unwrap();
        let accept = async { server.accept().await.unwrap().await.unwrap() };
        let (client_conn, server_conn) = tokio::join!(async { connecting.await.unwrap() }, accept);

        (client_conn, server_conn)
    }

    async fn http3_pair() -> (
        Session,
        noq::Connection,
        Connected,
        Settings,
        [noq::Endpoint; 2],
    ) {
        http3_pair_with(Default::default()).await
    }

    async fn http3_pair_with(
        peer_transport: noq::TransportConfig,
    ) -> (
        Session,
        noq::Connection,
        Connected,
        Settings,
        [noq::Endpoint; 2],
    ) {
        let (client, server, endpoints) =
            pair_with_transports(Default::default(), peer_transport).await;
        let (session, connect, settings) = http3_session(client, &server).await;
        (session, server, connect, settings, endpoints)
    }

    async fn http3_session(
        client: noq::Connection,
        server: &noq::Connection,
    ) -> (Session, Connected, Settings) {
        let client_session =
            Session::connect(client, url::Url::parse("https://localhost/").unwrap());
        let peer = async {
            let settings = Settings::connect(server).await.unwrap();
            let connect = crate::Connecting::accept(server)
                .await
                .unwrap()
                .respond(http::StatusCode::OK)
                .await
                .unwrap();
            (settings, connect)
        };
        let (session, (settings, connect)) = tokio::join!(client_session, peer);
        (session.unwrap(), connect, settings)
    }

    #[tokio::test]
    async fn http3_final_drop_closes_when_only_capsule_reader_holds_transport() {
        let (session, peer, _connect, _settings, _endpoints) = http3_pair().await;
        drop(session);
        tokio::time::timeout(Duration::from_secs(1), peer.closed())
            .await
            .expect("capsule reader must not retain the transport after final Session drop");
    }

    #[tokio::test]
    async fn http3_final_drop_closes_with_peer_connect_and_native_handles_retained() {
        let (session, peer, _connect, _settings, _endpoints) = http3_pair().await;
        let native = session.conn.clone();
        let _stream = session.open_uni().await.unwrap();
        let reader = session._owner.reader.as_ref().unwrap().clone();
        tokio::task::yield_now().await;
        // Final drop must also work away from a Tokio runtime.
        std::thread::spawn(move || drop(session)).join().unwrap();
        assert!(
            native.close_reason().is_some(),
            "final Session drop must close synchronously"
        );
        tokio::time::timeout(Duration::from_secs(1), peer.closed())
            .await
            .expect("peer must observe final Session drop");
        tokio::time::timeout(Duration::from_secs(1), async {
            while !reader.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("capsule reader must terminate after final drop");
    }

    #[tokio::test]
    async fn raw_final_drop_closes_with_stream_and_native_handle_retained() {
        let (client, peer, _endpoints) = pair().await;
        let native = client.clone();
        let session = Session::raw(client);
        let _stream = session.open_uni().await.unwrap();
        drop(session);
        assert!(
            native.close_reason().is_some(),
            "final raw Session drop must close synchronously"
        );
        tokio::time::timeout(Duration::from_secs(1), peer.closed())
            .await
            .expect("peer must observe final raw Session drop");
    }

    fn close_operation(session: &Session, code: u32, reason: &str) -> impl Future<Output = ()> {
        session
            .error
            .set(noq::ConnectionError::LocallyClosed.into())
            .unwrap();
        let send = session.connect_send.lock().unwrap().take().unwrap();
        Session::close_with_capsule(
            session.conn.clone(),
            send,
            code,
            reason.to_owned(),
            Duration::from_secs(60),
            session._owner.reader.clone(),
        )
    }

    async fn reader_finished(reader: &tokio::task::AbortHandle) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !reader.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("capsule reader must terminate after graceful close ends");
    }

    #[derive(Default)]
    struct FlagWake(AtomicBool);

    impl Wake for FlagWake {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn cancelled_close_before_first_poll_closes_and_wakes_waiters() {
        let (session, peer, _connect, _settings, _endpoints) = http3_pair().await;
        let native = session.conn.clone();
        let reader = session._owner.reader.as_ref().unwrap().clone();
        let wakes = [(); 3].map(|_| Arc::new(FlagWake::default()));
        let wakers = wakes.each_ref().map(|wake| Waker::from(wake.clone()));
        let [mut cx, mut datagram_cx, mut accept_cx] = wakers.each_ref().map(Context::from_waker);
        let mut closed = Box::pin(session.closed());
        let mut datagram = Box::pin(session.read_datagram());
        let mut accept = Box::pin(session.accept_uni());
        assert!(closed.as_mut().poll(&mut cx).is_pending());
        assert!(datagram.as_mut().poll(&mut datagram_cx).is_pending());
        assert!(accept.as_mut().poll(&mut accept_cx).is_pending());

        let close = close_operation(&session, 42, "first reason");
        session.close(43, b"second reason");
        for wake in &wakes {
            wake.0.store(false, Ordering::SeqCst);
        }
        drop(close);
        assert!(
            native.close_reason().is_some(),
            "unpolled close cancellation must close synchronously"
        );
        assert!(
            wakes.iter().all(|wake| wake.0.load(Ordering::SeqCst)),
            "native close must request every waiter wakeup synchronously"
        );
        assert!(matches!(
            closed.as_mut().poll(&mut cx),
            Poll::Ready(SessionError::ConnectionError(
                noq::ConnectionError::LocallyClosed
            ))
        ));
        assert!(matches!(
            datagram.as_mut().poll(&mut datagram_cx),
            Poll::Ready(Err(SessionError::ConnectionError(
                noq::ConnectionError::LocallyClosed
            )))
        ));
        assert!(matches!(
            accept.as_mut().poll(&mut accept_cx),
            Poll::Ready(Err(SessionError::ConnectionError(
                noq::ConnectionError::LocallyClosed
            )))
        ));
        let error = tokio::time::timeout(Duration::from_secs(1), peer.closed())
            .await
            .unwrap();
        assert!(
            matches!(error, noq::ConnectionError::ApplicationClosed(close)
            if close.error_code.into_inner() == web_transport_proto::error_to_http3(42)
                && close.reason == b"first reason"[..])
        );
        reader_finished(&reader).await;
    }

    #[tokio::test]
    async fn cancelled_close_reason_fits_minimum_native_close_budget() {
        let (session, peer, _connect, _settings, _endpoints) = http3_pair().await;
        // A character straddles byte 16, so the fallback must preserve UTF-8
        // while the complete reason remains available for the close capsule.
        let reason = format!("{}é{}", "x".repeat(15), "é".repeat(800));
        drop(close_operation(&session, 42, &reason));
        let closed = tokio::time::timeout(Duration::from_secs(1), peer.closed())
            .await
            .unwrap();
        let noq::ConnectionError::ApplicationClosed(close) = closed else {
            panic!("expected application close, got {closed:?}");
        };
        assert_eq!(
            close.error_code.into_inner(),
            web_transport_proto::error_to_http3(42)
        );
        let prefix = std::str::from_utf8(&close.reason).unwrap();
        assert!(!prefix.is_empty() && reason.starts_with(prefix));

        // Noq writes ACKs first and encodes close only when the remaining
        // frame budget exceeds ConnectionClose::SIZE_BOUND (1 + 8 + 8 + 8).
        // Encode the actually received fallback independently of Noq's buggy
        // truncation, against that minimum available space of 26 bytes.
        let mut frame = Vec::new();
        VarInt::from_u32(0x1d).encode(&mut frame);
        VarInt::try_from(close.error_code.into_inner())
            .unwrap()
            .encode(&mut frame);
        VarInt::try_from(close.reason.len())
            .unwrap()
            .encode(&mut frame);
        frame.extend_from_slice(&close.reason);
        assert!(
            frame.len() <= 26,
            "native fallback needs {} bytes, but ACKs can leave only 26",
            frame.len()
        );
    }

    #[tokio::test]
    async fn cancelled_close_during_blocked_capsule_write_closes_synchronously() {
        let mut transport = noq::TransportConfig::default();
        transport.stream_receive_window(1024u32.into());
        let (session, _peer, _connect, _settings, _endpoints) = http3_pair_with(transport).await;
        let native = session.conn.clone();
        let reader = session._owner.reader.as_ref().unwrap().clone();
        tokio::time::pause();
        let mut close = Box::pin(close_operation(&session, 42, &"blocked".repeat(2048)));
        assert!(futures::poll!(&mut close).is_pending());
        assert!(native.close_reason().is_none());
        drop(close);
        assert!(
            native.close_reason().is_some(),
            "blocked capsule cancellation must close synchronously"
        );
        tokio::time::resume();
        reader_finished(&reader).await;
    }

    #[tokio::test]
    async fn cancelled_close_while_waiting_for_peer_closes_and_releases_reader() {
        let (session, _peer, connect, _settings, _endpoints) = http3_pair().await;
        let native = session.conn.clone();
        let reader = session._owner.reader.as_ref().unwrap().clone();
        let owner = Arc::downgrade(&session._owner);
        let mut close = Box::pin(close_operation(&session, 42, "waiting"));
        assert!(futures::poll!(&mut close).is_pending());
        let mut capsules = web_transport_proto::Http3CapsuleReader::new(connect.recv);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), capsules.read())
                .await
                .unwrap()
                .unwrap(),
            Some(web_transport_proto::Capsule::CloseWebTransportSession { code: 42, .. })
        ));
        assert!(native.close_reason().is_none());
        drop(session);
        assert!(
            owner.upgrade().is_none(),
            "graceful close must not retain frontend ownership"
        );
        drop(close);
        assert!(
            native.close_reason().is_some(),
            "peer-wait cancellation must close synchronously"
        );
        reader_finished(&reader).await;
    }

    #[tokio::test]
    async fn completed_close_preserves_observed_peer_error() {
        let (session, peer, _connect, _settings, _endpoints) = http3_pair().await;
        let native = session.conn.clone();
        let mut close = Box::pin(close_operation(&session, 42, "local first"));
        assert!(futures::poll!(&mut close).is_pending());
        peer.close(123u32.into(), b"peer reason");
        native.closed().await;
        assert!(futures::poll!(&mut close).is_ready());
        assert!(
            matches!(native.close_reason(), Some(noq::ConnectionError::ApplicationClosed(close))
            if close.error_code == 123u32.into() && close.reason == b"peer reason"[..]),
            "graceful cleanup must not overwrite an observed native peer error"
        );
    }

    fn assert_shutdown_closes(yielded: bool) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (native, reader, resources, mut closed, wake, _held) = runtime.block_on(async {
            let mut transport = noq::TransportConfig::default();
            transport.stream_receive_window(1024u32.into());
            let (session, peer, connect, settings, endpoints) = http3_pair_with(transport).await;
            let native = session.conn.clone();
            let reader = session._owner.reader.as_ref().unwrap().clone();
            let resources = Arc::downgrade(&session.error);
            let conn = native.clone();
            let mut closed = Box::pin(async move { conn.closed().await });
            let wake = Arc::new(FlagWake::default());
            let waker = Waker::from(wake.clone());
            assert!(closed
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending());
            tokio::time::pause();
            session.close(42, &vec![b'x'; 8192]);
            drop(session);
            if yielded {
                tokio::task::yield_now().await;
            }
            assert!(native.close_reason().is_none());
            wake.0.store(false, Ordering::SeqCst);
            (
                native,
                reader,
                resources,
                closed,
                wake,
                (peer, connect, settings, endpoints),
            )
        });
        drop(runtime);
        assert!(
            native.close_reason().is_some(),
            "runtime shutdown must close locally (yielded={yielded})"
        );
        assert!(
            wake.0.load(Ordering::SeqCst),
            "runtime shutdown must request waiter wakeups"
        );
        assert!(reader.is_finished());
        assert!(
            resources.upgrade().is_none(),
            "runtime shutdown must release task-owned shared state"
        );
        assert!(matches!(
            closed
                .as_mut()
                .poll(&mut Context::from_waker(futures::task::noop_waker_ref())),
            Poll::Ready(noq::ConnectionError::LocallyClosed)
        ));
    }

    #[test]
    fn runtime_shutdown_closes_unpolled_graceful_task() {
        assert_shutdown_closes(false);
    }

    #[test]
    fn runtime_shutdown_closes_yielded_graceful_task() {
        assert_shutdown_closes(true);
    }

    #[tokio::test]
    async fn repeated_lifetimes_return_fixed_endpoints_to_baseline() {
        let endpoints = endpoints_with_transports(Default::default(), Default::default());
        let baseline = endpoints.each_ref().map(noq::Endpoint::open_connections);
        assert_eq!(baseline, [0, 0]);
        // Reuse both endpoints across every mode and cycle. Endpoint shutdown
        // would hide leaked connection drivers, so only wait for natural draining.
        for cycle in 0..3 {
            for mode in ["implicit", "explicit", "cancelled"] {
                let (client, peer) = connect_endpoints(&endpoints).await;
                let (session, mut connect, settings) = http3_session(client, &peer).await;
                let native = session.conn.clone();
                let weak_native = native.weak_handle();
                let owner = Arc::downgrade(&session._owner);
                let error = Arc::downgrade(&session.error);
                let reader = session._owner.reader.as_ref().unwrap().clone();
                let stream = session.open_uni().await.unwrap();
                match mode {
                    "explicit" => {
                        // The capsule keeps the complete reason, independently
                        // of the shorter native fallback carried by cancellation.
                        let reason = "é".repeat(800);
                        session.close(42, reason.as_bytes());
                        let mut capsules =
                            web_transport_proto::Http3CapsuleReader::new(&mut connect.recv);
                        let capsule = tokio::time::timeout(Duration::from_secs(1), capsules.read())
                            .await
                            .unwrap()
                            .unwrap();
                        assert!(
                            matches!(capsule, Some(web_transport_proto::Capsule::CloseWebTransportSession {code:42, reason:actual}) if actual == reason)
                        );
                        peer.close(
                            web_transport_proto::error_to_http3(42).try_into().unwrap(),
                            b"ack",
                        );
                        session.closed().await;
                    }
                    "cancelled" => {
                        let reason = format!("x{}", "é".repeat(800));
                        drop(close_operation(&session, 42, &reason));
                        assert!(native.close_reason().is_some());
                        let closed = tokio::time::timeout(Duration::from_secs(1), peer.closed())
                            .await
                            .unwrap();
                        assert!(
                            matches!(closed, noq::ConnectionError::ApplicationClosed(close)
                            if close.error_code.into_inner() == web_transport_proto::error_to_http3(42)
                                && close.reason.len() <= 16
                                && std::str::from_utf8(&close.reason).is_ok_and(|prefix| reason.starts_with(prefix)))
                        );
                    }
                    "implicit" => {}
                    _ => unreachable!(),
                }
                drop(session);
                assert!(
                    native.close_reason().is_some(),
                    "{mode} cycle {cycle} must close locally"
                );
                assert!(owner.upgrade().is_none());
                reader_finished(&reader).await;
                drop((stream, native, peer, connect, settings));
                // Real QUIC networking includes its draining timer. This bound
                // measures eventual driver release, not synchronous local close.
                tokio::time::timeout(Duration::from_secs(5), async {
                    tokio::join!(endpoints[0].wait_idle(), endpoints[1].wait_idle());
                })
                .await
                .expect("connection drivers must return to baseline");
                assert_eq!(
                    endpoints.each_ref().map(noq::Endpoint::open_connections),
                    baseline,
                    "{mode} cycle {cycle}"
                );
                assert!(!weak_native.is_alive());
                assert!(error.upgrade().is_none());
            }
        }
    }

    #[tokio::test]
    async fn graceful_close_deadline_includes_blocked_capsule_write() {
        let mut peer_transport = noq::TransportConfig::default();
        peer_transport.stream_receive_window(1024u32.into());
        let (session, _peer, _connect, _settings, _endpoints) =
            http3_pair_with(peer_transport).await;
        let send = session.connect_send.lock().unwrap().take().unwrap();
        tokio::time::pause();
        let deadline = Duration::from_millis(100);
        let expires = tokio::time::Instant::now() + deadline;
        let close = Session::close_with_capsule(
            session.conn.clone(),
            send,
            42,
            "blocked".repeat(1024),
            deadline,
            session._owner.reader.clone(),
        );
        tokio::pin!(close);
        assert!(futures::poll!(&mut close).is_pending());
        tokio::time::advance(deadline - Duration::from_millis(1)).await;
        assert!(futures::poll!(&mut close).is_pending());
        assert!(session.conn.close_reason().is_none());
        tokio::time::sleep_until(expires).await;
        assert!(
            futures::poll!(&mut close).is_ready(),
            "capsule flow control must not escape the deadline"
        );
        assert!(session.conn.close_reason().is_some());
    }

    #[tokio::test]
    async fn blocked_explicit_close_remains_bounded_after_final_drop() {
        let mut peer_transport = noq::TransportConfig::default();
        peer_transport.stream_receive_window(1024u32.into());
        let (session, _peer, _connect, _settings, _endpoints) =
            http3_pair_with(peer_transport).await;
        let native = session.conn.clone();
        let timeout = (native
            .rtt(noq::PathId::ZERO)
            .unwrap_or(Duration::from_millis(100))
            * 3)
        .max(Duration::from_millis(100));
        tokio::time::pause();
        let expires = tokio::time::Instant::now() + timeout;
        session.close(42, &vec![b'x'; 8192]);
        drop(session);
        assert!(native.close_reason().is_none());
        // Start the already-spawned close operation before advancing its timer.
        tokio::task::yield_now().await;
        tokio::time::sleep_until(expires).await;
        tokio::task::yield_now().await;
        assert!(
            native.close_reason().is_some(),
            "graceful close must finish after final drop"
        );
    }

    async fn assert_clone_survives(session: Session, peer: &noq::Connection) {
        let clone = session.clone();
        drop(session);
        assert!(clone.close_reason().is_none());
        clone.send_datagram(Bytes::from_static(b"alive")).unwrap();
        let datagram = tokio::time::timeout(Duration::from_secs(1), peer.read_datagram())
            .await
            .unwrap()
            .unwrap();
        assert!(datagram.ends_with(b"alive"));
        drop(clone);
        tokio::time::timeout(Duration::from_secs(1), peer.closed())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn one_clone_drop_preserves_http3_and_raw_sessions() {
        let (session, peer, _connect, _settings, _endpoints) = http3_pair().await;
        assert_clone_survives(session, &peer).await;
        let (client, peer, _endpoints) = pair().await;
        assert_clone_survives(Session::raw(client), &peer).await;
    }

    #[tokio::test]
    async fn explicit_close_survives_final_drop_and_preserves_first_capsule() {
        let (session, peer, connect, _settings, _endpoints) = http3_pair().await;
        let native = session.conn.clone();
        session.close(42, b"first reason");
        session.close(43, b"second reason");
        drop(session);
        assert!(
            native.close_reason().is_none(),
            "final drop must preserve pending graceful close"
        );
        let mut reader = web_transport_proto::Http3CapsuleReader::new(connect.recv);
        let capsule = tokio::time::timeout(Duration::from_secs(1), reader.read())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(capsule,
            web_transport_proto::Capsule::CloseWebTransportSession { code: 42, reason }
            if reason == "first reason"));
        let code = web_transport_proto::error_to_http3(42).try_into().unwrap();
        peer.close(code, b"acknowledged");
        tokio::time::timeout(Duration::from_secs(1), native.closed())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn explicit_raw_close_preserves_first_reason_after_final_drop() {
        let (client, peer, _endpoints) = pair().await;
        let session = Session::raw(client);
        session.close(42, b"first reason");
        session.close(43, b"second reason");
        drop(session);
        let error = tokio::time::timeout(Duration::from_secs(1), peer.closed())
            .await
            .unwrap();
        assert!(
            matches!(error, noq::ConnectionError::ApplicationClosed(close)
            if close.error_code == 42u32.into() && close.reason == b"first reason"[..])
        );
    }

    #[tokio::test]
    async fn final_drop_preserves_an_observed_native_remote_close() {
        let (client, peer, _endpoints) = pair().await;
        let native = client.clone();
        let session = Session::raw(client);
        let send = session.open_uni().await.unwrap();
        peer.close(42u32.into(), b"remote reason");
        tokio::time::timeout(Duration::from_secs(1), native.closed())
            .await
            .unwrap();
        drop(session);
        let error = send.stopped().await.unwrap_err();
        assert!(matches!(error,
            SessionError::ConnectionError(noq::ConnectionError::ApplicationClosed(close))
            if close.error_code == 42u32.into() && close.reason == b"remote reason"[..]));
    }

    fn assert_reset(err: SessionError) {
        assert!(
            matches!(
                &err,
                SessionError::WebTransportError(WebTransportError::ReadError(
                    noq::ReadExactError::ReadError(noq::ReadError::Reset(code))
                )) if code.into_inner() == 7
            ),
            "{err:?}"
        );
    }

    /// A uni stream reset before its header arrived reports the reset, not a foreign session.
    #[tokio::test]
    async fn uni_reset_before_header() {
        let (client, server, _endpoints) = pair().await;

        // The stream type alone, so the reset also cuts off the session ID.
        let mut send = client.open_uni().await.unwrap();
        send.write_all(&[0x40, 0x54]).await.unwrap();
        send.reset(7u32.into()).unwrap();

        let recv = server.accept_uni().await.unwrap();
        let err = SessionAccept::decode_uni(recv, SESSION)
            .await
            .err()
            .unwrap();
        assert_reset(err);
    }

    /// A bi stream reset before its header arrived reports the reset, not a foreign session.
    #[tokio::test]
    async fn bi_reset_before_header() {
        let (client, server, _endpoints) = pair().await;

        let (mut send, _recv) = client.open_bi().await.unwrap();
        send.reset(7u32.into()).unwrap();

        let (send, recv) = server.accept_bi().await.unwrap();
        let err = SessionAccept::decode_bi(send, recv, SESSION)
            .await
            .err()
            .unwrap();
        assert_reset(err);
    }

    /// A stream that names another session is still reported as `UnknownSession`.
    #[tokio::test]
    async fn uni_other_session() {
        let (client, server, _endpoints) = pair().await;

        let mut send = client.open_uni().await.unwrap();
        send.write_all(&[0x40, 0x54, 0x04]).await.unwrap();
        send.finish().unwrap();

        let recv = server.accept_uni().await.unwrap();
        let err = SessionAccept::decode_uni(recv, SESSION)
            .await
            .err()
            .unwrap();
        assert!(
            matches!(
                err,
                SessionError::WebTransportError(WebTransportError::UnknownSession)
            ),
            "{err:?}"
        );
    }

    /// A peer without datagram support reports a max datagram size of 0 instead of panicking.
    #[tokio::test]
    async fn no_datagram_support() {
        let mut transport = noq::TransportConfig::default();
        transport.datagram_receive_buffer_size(None);
        let (_client, server, _endpoints) = pair_with(transport).await;

        assert_eq!(Session::raw(server).max_datagram_size(), 0);
    }
}
