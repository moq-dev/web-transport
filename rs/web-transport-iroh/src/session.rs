use std::{
    fmt,
    future::Future,
    io::Cursor,
    ops::Deref,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use bytes::{Bytes, BytesMut};
use iroh::endpoint::{self, Connection, PathStats};
use kio::{Fan, Waiter};
use n0_future::{
    FuturesUnordered,
    stream::{Stream, StreamExt},
};
use tokio::sync::watch;
use web_transport_proto::{ConnectRequest, ConnectResponse, Frame, StreamUni, VarInt};

use crate::{
    ClientError, Connected, RecvStream, SendStream, SessionError, Settings, WebTransportError,
};

/// An established WebTransport session, acting like a full QUIC connection. See [`iroh::endpoint::Connection`].
///
/// It is important to remember that WebTransport is layered on top of QUIC:
///   1. Each stream starts with a few bytes identifying the stream type and session ID.
///   2. Errors codes are encoded with the session ID, so they aren't full QUIC error codes.
///   3. Stream IDs may have gaps in them, used by HTTP/3 transparent to the application.
///
/// Deref is used to expose non-overloaded methods on [`iroh::endpoint::Connection`].
/// These should be safe to use with WebTransport, but file a PR if you find one that isn't.
#[derive(Clone)]
pub struct Session {
    conn: Connection,
    h3: Option<H3SessionState>,
}

impl Session {
    /// Create a new session from a raw QUIC connection.
    ///
    /// This is used to pretend like a QUIC connection is a WebTransport session,
    /// making it easier to support WebTransport and raw QUIC simultaneously.
    ///
    /// There is no HTTP/3 exchange, so [`Self::request`] and [`Self::response`] both
    /// return `None`. [`Self::protocol`] reports the ALPN negotiated by the QUIC
    /// handshake instead.
    pub fn raw(conn: Connection) -> Self {
        Self { conn, h3: None }
    }

    /// Connect using an established QUIC connection if you want to create the connection yourself.
    /// This will only work with a brand new QUIC connection using the HTTP/3 ALPN.
    pub async fn connect_h3(
        conn: Connection,
        request: impl Into<ConnectRequest>,
    ) -> Result<Session, ClientError> {
        let request = request.into();

        // Perform the H3 handshake by sending/receiving SETTINGS frames.
        let settings = Settings::connect(&conn).await?;

        // Send the HTTP/3 CONNECT request.
        let connect = Connected::open(&conn, request).await?;

        // Return the resulting session with a reference to the control/connect streams.
        // If either stream is closed, then the session will be closed, so we need to keep them around.
        let session = Session::new_h3(conn, settings, connect);

        Ok(session)
    }

    /// Creates a session from pre-established HTTP/3 handshake components.
    pub fn new_h3(conn: Connection, settings: Settings, mut connect: Connected) -> Self {
        // The sender lives in the task below, so `closed` can wait for it to finish.
        let (peer_close, peer_close_rx) = watch::channel(None);
        let h3 = H3SessionState::connect(conn.clone(), settings, &connect, peer_close_rx);
        let this = Session { conn, h3: Some(h3) };
        // Run a background task to check if the connect stream is closed.
        let this2 = this.clone();
        tokio::spawn(async move {
            let closed = connect.run_closed().await;
            let close_reason = this2.conn().close_reason();
            // A peer may close the connection right after its capsule, so record the
            // capsule even then; only an earlier local close wins over it.
            if let Ok(Some((code, reason))) = &closed
                && !matches!(close_reason, Some(endpoint::ConnectionError::LocallyClosed))
            {
                let err = WebTransportError::Closed {
                    code: *code,
                    reason: reason.clone(),
                };
                peer_close.send_replace(Some(err.into()));
            }
            // A connection that is already closed has its own reason.
            if close_reason.is_some() {
                return;
            }
            let (code, reason) = match closed {
                Ok(Some(close)) => close,
                Ok(None) => (0, "stream closed".to_string()),
                Err(err) => {
                    tracing::warn!(?err, "failed to read capsule");
                    (1, "capsule error".to_string())
                }
            };
            // TODO We shouldn't be closing the QUIC connection with the same error.
            this2.close(code, reason.as_bytes());
        });
        this
    }

    /// Returns the underlying QUIC connection.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Returns the [`ConnectRequest`] if this session was established over HTTP/3.
    pub fn request(&self) -> Option<&ConnectRequest> {
        self.h3.as_ref().map(|s| &s.request)
    }

    /// Returns the [`ConnectResponse`] if this session was established over HTTP/3.
    pub fn response(&self) -> Option<&ConnectResponse> {
        self.h3.as_ref().map(|s| &s.response)
    }

    /// Returns the application protocol negotiated for this session.
    ///
    /// For an HTTP/3 session this is the subprotocol the server selected via
    /// `WT-Protocol`; for a raw QUIC session it is the negotiated ALPN.
    /// Returns `None` if neither was negotiated or the ALPN is not valid UTF-8.
    pub fn protocol(&self) -> Option<&str> {
        match self.h3.as_ref() {
            None => std::str::from_utf8(self.conn.alpn()).ok(),
            Some(h3) => h3.response.protocol.as_deref(),
        }
    }

    /// Accept a new unidirectional stream. See [`iroh::endpoint::Connection::accept_uni`].
    pub async fn accept_uni(&self) -> Result<RecvStream, SessionError> {
        if let Some(h3) = &self.h3 {
            // `kio::wait` owns the waiter, so dropping this future — a `timeout` that
            // expires, say — also drops its registration in `H3SessionAccept`.
            kio::wait(|waiter| poll_accept_uni_shared(&h3.accept, waiter)).await
        } else {
            self.conn
                .accept_uni()
                .await
                .map(|recv| RecvStream::new(recv, true))
                .map_err(Into::into)
        }
    }

    /// Accept a new bidirectional stream. See [`iroh::endpoint::Connection::accept_bi`].
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        if let Some(h3) = &self.h3 {
            kio::wait(|waiter| poll_accept_bi_shared(&h3.accept, waiter)).await
        } else {
            self.conn
                .accept_bi()
                .await
                .map(|(send, recv)| (SendStream::new(send, true), RecvStream::new(recv, true)))
                .map_err(Into::into)
        }
    }

    /// Open a new unidirectional stream. See [`iroh::endpoint::Connection::open_uni`].
    pub async fn open_uni(&self) -> Result<SendStream, SessionError> {
        let mut send = self.conn.open_uni().await?;

        if let Some(h3) = self.h3.as_ref() {
            write_full_with_max_prio(&mut send, &h3.header_uni).await?;
        }

        Ok(SendStream::new(send, self.h3.is_none()))
    }

    /// Open a new bidirectional stream. See [`iroh::endpoint::Connection::open_bi`].
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        let (mut send, recv) = self.conn.open_bi().await?;

        if let Some(h3) = self.h3.as_ref() {
            write_full_with_max_prio(&mut send, &h3.header_bi).await?;
        }

        let raw = self.h3.is_none();
        Ok((SendStream::new(send, raw), RecvStream::new(recv, raw)))
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
            .map_err(SessionError::from)?;

        let datagram = if let Some(h3) = self.h3.as_ref() {
            let mut cursor = Cursor::new(&datagram);

            // We have to check and strip the session ID from the datagram.
            let actual_id =
                VarInt::decode(&mut cursor).map_err(|_| WebTransportError::UnknownSession)?;
            if actual_id != h3.session_id {
                return Err(WebTransportError::UnknownSession.into());
            }

            // Return the datagram without the session ID.

            datagram.split_off(cursor.position() as usize)
        } else {
            datagram
        };

        Ok(datagram)
    }

    /// Sends an application datagram to the remote peer.
    ///
    /// Datagrams are unreliable and may be dropped or delivered out of order.
    /// The data must be smaller than [`max_datagram_size`](Self::max_datagram_size).
    pub fn send_datagram(&self, data: Bytes) -> Result<(), SessionError> {
        let datagram = if let Some(h3) = self.h3.as_ref() {
            // Unfortunately, we need to allocate/copy each datagram because of the Quinn API.
            // https://github.com/quinn-rs/quinn/issues/1724
            let mut buf = BytesMut::with_capacity(h3.header_datagram.len() + data.len());
            // Prepend the datagram with the header indicating the session ID.
            buf.extend_from_slice(&h3.header_datagram);
            buf.extend_from_slice(&data);
            buf.into()
        } else {
            data
        };

        self.conn.send_datagram(datagram)?;

        Ok(())
    }

    /// Computes the maximum size of datagrams that may be passed to
    /// [`send_datagram`](Self::send_datagram), or 0 when the peer does not accept datagrams.
    pub fn max_datagram_size(&self) -> usize {
        let mtu = self.conn.max_datagram_size().unwrap_or(0);
        if let Some(h3) = self.h3.as_ref() {
            mtu.saturating_sub(h3.header_datagram.len())
        } else {
            mtu
        }
    }

    /// Immediately close the connection with an error code and reason. See [`iroh::endpoint::Connection::close`].
    pub fn close(&self, code: u32, reason: &[u8]) {
        let code = if self.h3.is_some() {
            web_transport_proto::error_to_http3(code)
                .try_into()
                .unwrap()
        } else {
            code.into()
        };

        self.conn.close(code, reason)
    }

    /// Wait until the session is closed, returning the error. See [`iroh::endpoint::Connection::closed`].
    ///
    /// A peer's `CloseWebTransportSession` capsule is reported as [`WebTransportError::Closed`].
    pub async fn closed(&self) -> SessionError {
        let err = self.conn.closed().await;
        if let Some(h3) = &self.h3 {
            // The CONNECT stream may still hold the peer's capsule; its reader ends
            // promptly once the connection is closed.
            let mut peer_close = h3.peer_close.clone();
            while peer_close.changed().await.is_ok() {}
        }
        self.peer_close().unwrap_or_else(|| err.into())
    }

    /// Return why the session was closed, or None if it's not closed. See [`iroh::endpoint::Connection::close_reason`].
    pub fn close_reason(&self) -> Option<SessionError> {
        let err = self.conn.close_reason()?;
        // The close is unsettled until the CONNECT stream reader finishes, since it may
        // still hold the peer's capsule.
        if let Some(h3) = &self.h3
            && h3.peer_close.has_changed().is_ok()
        {
            return None;
        }
        Some(self.peer_close().unwrap_or_else(|| err.into()))
    }

    fn peer_close(&self) -> Option<SessionError> {
        self.h3.as_ref()?.peer_close.borrow().clone()
    }
}

async fn write_full_with_max_prio(
    send: &mut endpoint::SendStream,
    buf: &[u8],
) -> Result<(), SessionError> {
    // Set the stream priority to max and then write the stream header.
    // Otherwise the application could write data with lower priority than the header, resulting in queuing.
    // Also the header is very important for determining the session ID without reliable reset.
    send.set_priority(i32::MAX).ok();
    let res = match send.write_all(buf).await {
        Ok(_) => Ok(()),
        Err(endpoint::WriteError::ConnectionLost(err)) => Err(err.into()),
        Err(err) => Err(WebTransportError::WriteError(err).into()),
    };
    // Reset the stream priority back to the default of 0.
    send.set_priority(0).ok();
    res
}

impl Deref for Session {
    type Target = Connection;

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

#[derive(Clone)]
struct H3SessionState {
    // The session ID, as determined by the stream ID of the connect request.
    session_id: VarInt,
    // Cache the headers in front of each stream we open.
    header_uni: Vec<u8>,
    header_bi: Vec<u8>,
    header_datagram: Vec<u8>,

    // Keep a reference to the settings and connect stream to avoid closing them until dropped.
    #[allow(unused)]
    settings: Arc<Settings>,
    // The accept logic is stateful, so use an Arc<Mutex> to share it.
    accept: Arc<Mutex<H3SessionAccept>>,

    // The peer's CloseWebTransportSession capsule. Our connection close echoes it, but
    // that would report the close as local, without the code or reason.
    peer_close: watch::Receiver<Option<SessionError>>,

    // The request sent by the client.
    request: ConnectRequest,

    // The response sent by the server.
    response: ConnectResponse,
}

impl fmt::Debug for H3SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("H3SessionState")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl H3SessionState {
    fn connect(
        conn: Connection,
        settings: Settings,
        connect: &Connected,
        peer_close: watch::Receiver<Option<SessionError>>,
    ) -> Self {
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

        // Accept logic is stateful, so use an Arc<Mutex> to share it.
        let accept = H3SessionAccept::new(conn, session_id);
        Self {
            session_id,
            header_uni,
            header_bi,
            header_datagram,
            settings: Arc::new(settings),
            accept: Arc::new(Mutex::new(accept)),
            peer_close,
            request: connect.request.clone(),
            response: connect.response.clone(),
        }
    }
}

// Type aliases just so clippy doesn't complain about the complexity.
type AcceptUni = dyn Stream<Item = Result<endpoint::RecvStream, endpoint::ConnectionError>> + Send;
type AcceptBi = dyn Stream<Item = Result<(endpoint::SendStream, endpoint::RecvStream), endpoint::ConnectionError>>
    + Send;
type PendingUni =
    dyn Future<Output = Result<(StreamUni, endpoint::RecvStream), SessionError>> + Send;
type PendingBi = dyn Future<Output = Result<Option<(endpoint::SendStream, endpoint::RecvStream)>, SessionError>>
    + Send;

// Poll the shared accept state, then wake the *other* accepters once the lock is
// released.
//
// `H3SessionAccept` does not wake them itself: a waker is free to resume its task
// inline, and the first thing a resumed accepter does is take this same lock. An
// arrival or a failure is exactly what the others are parked waiting to retry
// after, so `Ready` is the signal.
fn poll_accept_uni_shared(
    accept: &Mutex<H3SessionAccept>,
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
    accept: &Mutex<H3SessionAccept>,
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

// Logic just for accepting streams, which is annoying because of the stream header.
struct H3SessionAccept {
    session_id: VarInt,

    // We also need to keep a reference to the qpack streams if the endpoint (incorrectly) creates them.
    // Again, this is just so they don't get closed until we drop the session.
    qpack_encoder: Option<endpoint::RecvStream>,
    qpack_decoder: Option<endpoint::RecvStream>,

    accept_uni: Pin<Box<AcceptUni>>,
    accept_bi: Pin<Box<AcceptBi>>,

    // Keep track of work being done to read/write the WebTransport stream header.
    pending_uni: FuturesUnordered<Pin<Box<PendingUni>>>,
    pending_bi: FuturesUnordered<Pin<Box<PendingBi>>>,

    // Waiters from concurrent callers of accept_bi / accept_uni.
    // Every clone of the session polls this one struct, so an arrival has to be fanned
    // out: each caller registers here and all of them are woken when a stream lands —
    // by the caller that saw it, once it has released the lock on this struct.
    bi_waiters: Fan,
    uni_waiters: Fan,

    // `Waker::from(waiters.clone())`, cached so the inner accept futures are polled with
    // the same waker every time. That waker outlives every caller, so an accepter that
    // drops its future cannot take the wakeup path with it, and the layer below holds
    // one registration rather than one per caller.
    bi_waker: Waker,
    uni_waker: Waker,
}

impl H3SessionAccept {
    pub(crate) fn new(conn: Connection, session_id: VarInt) -> Self {
        // Create a stream that just outputs new streams, so it's easy to call from poll.
        let accept_uni = Box::pin(n0_future::stream::unfold(conn.clone(), |conn| async {
            Some((conn.accept_uni().await, conn))
        }));

        let accept_bi = Box::pin(n0_future::stream::unfold(conn, |conn| async {
            Some((conn.accept_bi().await, conn))
        }));

        let bi_waiters = Fan::new();
        let uni_waiters = Fan::new();
        let bi_waker = bi_waiters.waker();
        let uni_waker = uni_waiters.waker();

        Self {
            session_id,

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
            if let Poll::Ready(Some(res)) = self.accept_uni.poll_next(cx) {
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
            let (typ, recv) = match self.pending_uni.poll_next(cx) {
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
                    let recv = RecvStream::new(recv, false);
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
                    tracing::debug!("ignoring unknown unidirectional stream: {typ:?}");
                }
            }
        }
    }

    // Reads the stream header, returning the stream type.
    async fn decode_uni(
        mut recv: endpoint::RecvStream,
        expected_session: VarInt,
    ) -> Result<(StreamUni, endpoint::RecvStream), SessionError> {
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
            if let Poll::Ready(Some(res)) = self.accept_bi.poll_next(cx) {
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
            let res = match self.pending_bi.poll_next(cx) {
                Poll::Ready(Some(Ok(res))) => res,
                Poll::Ready(Some(Err(err))) => {
                    log_header_error(err, "bidirectional");
                    continue;
                }
                Poll::Ready(None) | Poll::Pending => return Poll::Pending,
            };

            if let Some((send, recv)) = res {
                // Wrap the streams in our own types for correct error codes.
                let send = SendStream::new(send, false);
                let recv = RecvStream::new(recv, false);
                return Poll::Ready(Ok((send, recv)));
            }

            // Keep looping if it's a stream we want to ignore.
        }
    }

    // Reads the stream header, returning Some if it's a WebTransport stream.
    async fn decode_bi(
        send: endpoint::SendStream,
        mut recv: endpoint::RecvStream,
        expected_session: VarInt,
    ) -> Result<Option<(endpoint::SendStream, endpoint::RecvStream)>, SessionError> {
        let typ = read_varint(&mut recv).await?;
        if Frame(typ) != Frame::WEBTRANSPORT {
            tracing::debug!("ignoring unknown bidirectional stream: {typ:?}");
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
async fn read_varint(recv: &mut endpoint::RecvStream) -> Result<VarInt, WebTransportError> {
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
            endpoint::ReadExactError::ReadError(
                endpoint::ReadError::Reset(_) | endpoint::ReadError::ConnectionLost(_),
            ),
        )) => tracing::debug!(?err, direction, "stream closed before its header"),
        _ => tracing::warn!(?err, direction, "failed to decode stream header"),
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

    fn stats(&self) -> impl web_transport_trait::Stats {
        let selected_path_stats = self
            .conn
            .paths()
            .iter()
            .find(|p| p.is_selected())
            .map(|p| p.stats());
        SessionStats {
            stats: self.conn.stats(),
            selected_path_stats,
        }
    }
}

pub struct SessionStats {
    stats: iroh::endpoint::ConnectionStats,
    selected_path_stats: Option<PathStats>,
}

impl web_transport_trait::Stats for SessionStats {
    fn bytes_sent(&self) -> Option<u64> {
        Some(self.stats.udp_tx.bytes)
    }

    fn bytes_received(&self) -> Option<u64> {
        Some(self.stats.udp_rx.bytes)
    }

    fn bytes_lost(&self) -> Option<u64> {
        Some(self.stats.lost_bytes)
    }

    fn packets_sent(&self) -> Option<u64> {
        Some(self.stats.udp_tx.datagrams)
    }

    fn packets_received(&self) -> Option<u64> {
        Some(self.stats.udp_rx.datagrams)
    }

    fn packets_lost(&self) -> Option<u64> {
        Some(self.stats.lost_packets)
    }

    fn rtt(&self) -> Option<std::time::Duration> {
        self.selected_path_stats.map(|p| p.rtt)
    }

    fn estimated_send_rate(&self) -> Option<u64> {
        let path_stats = self.selected_path_stats?;
        let rtt_secs = path_stats.rtt.as_secs_f64();
        if path_stats.cwnd > 0 && rtt_secs > 0.0 {
            Some((path_stats.cwnd as f64 * 8.0 / rtt_secs) as u64)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use iroh::{
        Endpoint,
        endpoint::{QuicTransportConfig, presets},
    };

    use super::*;

    const ALPN: &[u8] = b"test";
    const SESSION: VarInt = VarInt::from_u32(0);

    /// A connected client and server connection, plus the endpoints that drive them.
    async fn pair() -> (Connection, Connection, [Endpoint; 2]) {
        pair_with(Default::default()).await
    }

    /// [`pair`], with the client using `transport`.
    async fn pair_with(transport: QuicTransportConfig) -> (Connection, Connection, [Endpoint; 2]) {
        let client = Endpoint::builder(presets::Minimal)
            .transport_config(transport)
            .bind()
            .await
            .unwrap();
        let server = Endpoint::builder(presets::Minimal)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .unwrap();

        let accept = async { server.accept().await.unwrap().await.unwrap() };
        let (client_conn, server_conn) = tokio::join!(
            async { client.connect(server.addr(), ALPN).await.unwrap() },
            accept
        );

        (client_conn, server_conn, [client, server])
    }

    fn assert_reset(err: SessionError) {
        assert!(
            matches!(
                &err,
                SessionError::WebTransportError(WebTransportError::ReadError(
                    endpoint::ReadExactError::ReadError(endpoint::ReadError::Reset(code))
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
        let err = H3SessionAccept::decode_uni(recv, SESSION)
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
        let err = H3SessionAccept::decode_bi(send, recv, SESSION)
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
        let err = H3SessionAccept::decode_uni(recv, SESSION)
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
        let transport = QuicTransportConfig::builder()
            .datagram_receive_buffer_size(None)
            .build();
        let (_client, server, _endpoints) = pair_with(transport).await;

        assert_eq!(Session::raw(server).max_datagram_size(), 0);
    }
}
