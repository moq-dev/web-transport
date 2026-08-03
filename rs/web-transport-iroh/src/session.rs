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
use kio::Waiter;
use n0_future::{
    FuturesUnordered,
    stream::{Stream, StreamExt},
};
use web_transport_proto::{ConnectRequest, ConnectResponse, Frame, StreamUni, VarInt};

use crate::{
    ClientError, Connected, RecvStream, SendStream, SessionError, Settings, WebTransportError,
    waiters::AcceptWaiters,
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
        let h3 = H3SessionState::connect(conn.clone(), settings, &connect);
        let this = Session { conn, h3: Some(h3) };
        // Run a background task to check if the connect stream is closed.
        let this2 = this.clone();
        tokio::spawn(async move {
            let (code, reason) = connect.run_closed().await;
            if this2.conn().close_reason().is_none() {
                // TODO We shouldn't be closing the QUIC connection with the same error.
                this2.close(code, reason.as_bytes());
            }
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
                .map(RecvStream::new)
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
                .map(|(send, recv)| (SendStream::new(send), RecvStream::new(recv)))
                .map_err(Into::into)
        }
    }

    /// Open a new unidirectional stream. See [`iroh::endpoint::Connection::open_uni`].
    pub async fn open_uni(&self) -> Result<SendStream, SessionError> {
        let mut send = self.conn.open_uni().await?;

        if let Some(h3) = self.h3.as_ref() {
            write_full_with_max_prio(&mut send, &h3.header_uni).await?;
        }

        Ok(SendStream::new(send))
    }

    /// Open a new bidirectional stream. See [`iroh::endpoint::Connection::open_bi`].
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        let (mut send, recv) = self.conn.open_bi().await?;

        if let Some(h3) = self.h3.as_ref() {
            write_full_with_max_prio(&mut send, &h3.header_bi).await?;
        }

        Ok((SendStream::new(send), RecvStream::new(recv)))
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
    /// [`send_datagram`](Self::send_datagram).
    pub fn max_datagram_size(&self) -> usize {
        let mtu = self
            .conn
            .max_datagram_size()
            .expect("datagram support is required");
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
    pub async fn closed(&self) -> SessionError {
        self.conn.closed().await.into()
    }

    /// Return why the session was closed, or None if it's not closed. See [`iroh::endpoint::Connection::close_reason`].
    pub fn close_reason(&self) -> Option<SessionError> {
        self.conn.close_reason().map(Into::into)
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
    fn connect(conn: Connection, settings: Settings, connect: &Connected) -> Self {
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
    let (result, waiters) = {
        let mut accept = accept.lock().unwrap();
        let waiters = accept.uni_waiters.clone();

        // The poll below drives the shared accept futures with this list's waker, and
        // one of them may wake it inline. Hold those back until the lock is gone.
        waiters.arm();
        let result = accept.poll_accept_uni(waiter);

        (result, waiters)
    };

    // `disarm` runs first either way: the count has to come down even on a `Ready`.
    if waiters.disarm() || result.is_ready() {
        waiters.wake_all();
    }

    result
}

fn poll_accept_bi_shared(
    accept: &Mutex<H3SessionAccept>,
    waiter: &Waiter,
) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
    let (result, waiters) = {
        let mut accept = accept.lock().unwrap();
        let waiters = accept.bi_waiters.clone();

        // The poll below drives the shared accept futures with this list's waker, and
        // one of them may wake it inline. Hold those back until the lock is gone.
        waiters.arm();
        let result = accept.poll_accept_bi(waiter);

        (result, waiters)
    };

    // `disarm` runs first either way: the count has to come down even on a `Ready`.
    if waiters.disarm() || result.is_ready() {
        waiters.wake_all();
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
    bi_waiters: Arc<AcceptWaiters>,
    uni_waiters: Arc<AcceptWaiters>,

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

        let bi_waiters = Arc::new(AcceptWaiters::default());
        let uni_waiters = Arc::new(AcceptWaiters::default());
        let bi_waker = Waker::from(bi_waiters.clone());
        let uni_waker = Waker::from(uni_waiters.clone());

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
                    // Ignore the error, the stream was probably reset early.
                    tracing::warn!("failed to decode unidirectional stream: {err:?}");
                    continue;
                }
                Poll::Ready(None) | Poll::Pending => return Poll::Pending,
            };

            // Decide if we keep looping based on the type.
            match typ {
                StreamUni::WEBTRANSPORT => {
                    let recv = RecvStream::new(recv);
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
        let typ = VarInt::read(&mut recv)
            .await
            .map_err(|_| WebTransportError::UnknownSession)?;
        let typ = StreamUni(typ);

        if typ == StreamUni::WEBTRANSPORT {
            // Read the session_id and validate it
            let session_id = VarInt::read(&mut recv)
                .await
                .map_err(|_| WebTransportError::UnknownSession)?;
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
                    // Ignore the error, the stream was probably reset early.
                    tracing::warn!("failed to decode bidirectional stream: {err:?}");
                    continue;
                }
                Poll::Ready(None) | Poll::Pending => return Poll::Pending,
            };

            if let Some((send, recv)) = res {
                // Wrap the streams in our own types for correct error codes.
                let send = SendStream::new(send);
                let recv = RecvStream::new(recv);
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
        let typ = VarInt::read(&mut recv)
            .await
            .map_err(|_| WebTransportError::UnknownSession)?;
        if Frame(typ) != Frame::WEBTRANSPORT {
            tracing::debug!("ignoring unknown bidirectional stream: {typ:?}");
            return Ok(None);
        }

        // Read the session ID and validate it.
        let session_id = VarInt::read(&mut recv)
            .await
            .map_err(|_| WebTransportError::UnknownSession)?;
        if session_id != expected_session {
            return Err(WebTransportError::UnknownSession.into());
        }

        Ok(Some((send, recv)))
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
