use crate::{
    ez, h3,
    waiters::{AcceptWaiters, Parked},
    ClientError, RecvStream, SendStream, SessionError,
};

use bytes::{Bytes, BytesMut};
use futures::{stream::FuturesUnordered, Stream, StreamExt};
use kio::Waiter;
use web_transport_proto::{ConnectRequest, ConnectResponse, Frame, StreamUni, VarInt};

use std::{
    future::Future,
    io::Cursor,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{ready, Context, Poll, Waker},
};

// "conn" in ascii; if you see this then close(code)
// hex: 0x636E6E6F, or 0x52E50ACE926F as an HTTP error code
// decimal: 1668181615, or 91143682298479 as an HTTP error code
const DROP_CODE: u64 = web_transport_proto::error_to_http3(0x636E6E6F);

struct ConnectionDrop {
    conn: ez::Connection,
}

impl Drop for ConnectionDrop {
    fn drop(&mut self) {
        if !self.conn.is_closed() {
            tracing::warn!("connection dropped without calling `close`");
            self.conn.close(DROP_CODE, "connection dropped");
        }
    }
}

/// An established WebTransport session, acting like a full QUIC connection.
///
/// It is important to remember that WebTransport is layered on top of QUIC:
///   1. Each stream starts with a few bytes identifying the stream type and session ID.
///   2. Error codes are encoded with the session ID, so they aren't full QUIC error codes.
///   3. Stream IDs may have gaps in them, used by HTTP/3 transparent to the application.
#[derive(Clone)]
pub struct Connection {
    conn: ez::Connection,

    // Dropped when all references are dropped.
    #[allow(dead_code)]
    drop: Arc<ConnectionDrop>,

    // The session ID, as determined by the stream ID of the connect request.
    session_id: Option<VarInt>,

    // The accept logic is stateful, so use an Arc<Mutex> to share it.
    accept: Option<Arc<Mutex<SessionAccept>>>,

    // Cache the headers in front of each stream we open.
    header_uni: Vec<u8>,
    header_bi: Vec<u8>,
    #[allow(unused)]
    header_datagram: Vec<u8>,

    // Keep a reference to the settings and connect stream to avoid closing them until dropped.
    #[allow(dead_code)]
    settings: Option<Arc<h3::Settings>>,

    // The request and response that were sent and received.
    // The request is None for a raw QUIC session.
    request: Option<ConnectRequest>,
    response: ConnectResponse,

    // Opening a stream is two steps — take the stream, then write the WebTransport
    // header — so a `Pending` in the middle has to resume rather than start over.
    //
    // A plain state machine, not a retained future: `ez` exposes every step as a
    // `poll_*`, so there is nothing to box. Per-clone, matching the poll trait's
    // "clone the session for concurrency".
    open_uni: OpenUni,
    open_bi: OpenBi,

    // Both `SessionAccept` and `ez` hold their waiters weakly, so the handle this clone
    // registered with has to outlive the poll that made it: one cell per operation,
    // per-clone, like the state above.
    parked_accept_uni: Parked,
    parked_accept_bi: Parked,
    parked_open_uni: Parked,
    parked_open_bi: Parked,
    parked_recv_datagram: Parked,
    parked_closed: Parked,
}

/// Where an in-progress `poll_open_uni` got to.
#[derive(Default)]
enum OpenUni {
    #[default]
    Idle,
    /// Stream taken, header partially written.
    Header { send: ez::SendStream, offset: usize },
}

/// Where an in-progress `poll_open_bi` got to.
#[derive(Default)]
enum OpenBi {
    #[default]
    Idle,
    Header {
        send: ez::SendStream,
        recv: ez::RecvStream,
        offset: usize,
    },
}

impl Clone for OpenUni {
    /// A clone starts idle; the operation in flight stays with the handle that
    /// started it.
    fn clone(&self) -> Self {
        Self::Idle
    }
}

impl Clone for OpenBi {
    fn clone(&self) -> Self {
        Self::Idle
    }
}

impl Connection {
    pub(super) fn new(
        conn: ez::Connection,
        settings: h3::Settings,
        connect: h3::Connected,
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
        let accept = SessionAccept::new(conn.clone(), session_id);

        let drop = Arc::new(ConnectionDrop { conn: conn.clone() });

        let this = Self {
            conn,
            drop,
            accept: Some(Arc::new(Mutex::new(accept))),
            session_id: Some(session_id),
            header_uni,
            header_bi,
            header_datagram,
            request: Some(connect.request.clone()),
            response: connect.response.clone(),
            settings: Some(Arc::new(settings)),
            open_uni: OpenUni::Idle,
            open_bi: OpenBi::Idle,
            parked_accept_uni: Parked::default(),
            parked_accept_bi: Parked::default(),
            parked_open_uni: Parked::default(),
            parked_open_bi: Parked::default(),
            parked_recv_datagram: Parked::default(),
            parked_closed: Parked::default(),
        };

        tracing::debug!(url = %connect.request.url, "WebTransport connection established");

        // Run a background task to check if the connect stream is closed.
        tokio::spawn(this.clone().run_closed(connect));

        this
    }

    // Keep reading from the control stream until it's closed.
    async fn run_closed(self, mut connect: h3::Connected) {
        loop {
            match web_transport_proto::Capsule::read(&mut connect.recv).await {
                Ok(Some(web_transport_proto::Capsule::CloseWebTransportSession {
                    code,
                    reason,
                })) => {
                    // TODO We shouldn't be closing the QUIC connection with the same error.
                    // Instead, we should return it to the application.
                    self.close(code, &reason);
                    return;
                }
                Ok(Some(web_transport_proto::Capsule::Grease { .. })) => {}
                Ok(Some(web_transport_proto::Capsule::Unknown { typ, payload })) => {
                    tracing::warn!("unknown capsule: type={typ} size={}", payload.len());
                }
                Ok(None) => {
                    // Stream closed without capsule
                    return;
                }
                Err(_) => {
                    self.close(500, "capsule error");
                    return;
                }
            }
        }
    }

    /// Connect using an established QUIC connection if you want to create the connection yourself.
    ///
    /// This will only work with a brand new QUIC connection using the HTTP/3 ALPN.
    pub async fn connect(
        conn: ez::Connection,
        request: impl Into<ConnectRequest>,
    ) -> Result<Connection, ClientError> {
        // Perform the H3 handshake by sending/reciving SETTINGS frames.
        let settings = h3::Settings::connect(&conn).await?;

        // Send the HTTP/3 CONNECT request.
        let connect = h3::Connected::open(&conn, request).await?;

        // Return the resulting session with a reference to the control/connect streams.
        // If either stream is closed, then the session will be closed, so we need to keep them around.
        let session = Connection::new(conn, settings, connect);

        Ok(session)
    }

    /// Accept a new unidirectional stream.
    ///
    /// Waits for a new incoming unidirectional stream from the remote peer.
    /// Returns a [RecvStream] that can be used to read data from the stream.
    pub async fn accept_uni(&self) -> Result<RecvStream, SessionError> {
        if let Some(accept) = &self.accept {
            // `kio::wait` owns the waiter, so dropping this future — a `timeout` that
            // expires, say — also drops its registration in `SessionAccept`.
            kio::wait(|waiter| poll_accept_uni_shared(accept, waiter)).await
        } else {
            self.conn
                .accept_uni()
                .await
                .map(RecvStream::new)
                .map_err(Into::into)
        }
    }

    /// Accept a new bidirectional stream.
    ///
    /// Waits for a new incoming bidirectional stream from the remote peer.
    /// Returns a ([SendStream], [RecvStream]) pair for sending and receiving data.
    pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        if let Some(accept) = &self.accept {
            kio::wait(|waiter| poll_accept_bi_shared(accept, waiter)).await
        } else {
            self.conn
                .accept_bi()
                .await
                .map(|(send, recv)| (SendStream::new(send), RecvStream::new(recv)))
                .map_err(Into::into)
        }
    }

    /// Open a new unidirectional stream.
    ///
    /// Creates a new outgoing unidirectional stream to the remote peer.
    /// Returns a [SendStream] that can be used to send data.
    pub async fn open_uni(&self) -> Result<SendStream, SessionError> {
        let mut send = self.conn.open_uni().await?;

        send.write_all(&self.header_uni)
            .await
            .map_err(SessionError::Header)?;

        Ok(SendStream::new(send))
    }

    /// Open a new bidirectional stream.
    ///
    /// Creates a new outgoing bidirectional stream to the remote peer.
    /// Returns a ([SendStream], [RecvStream]) pair for sending and receiving data.
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        let (mut send, recv) = self.conn.open_bi().await?;

        send.write_all(&self.header_bi)
            .await
            .map_err(SessionError::Header)?;

        Ok((SendStream::new(send), RecvStream::new(recv)))
    }

    /// Asynchronously receives an application datagram from the remote peer.
    ///
    /// This method is used to receive an application datagram sent by the remote
    /// peer over the connection.
    /// It waits for a datagram to become available and returns the received bytes.
    pub async fn read_datagram(&self) -> Result<Bytes, SessionError> {
        let datagram = self
            .conn
            .read_datagram()
            .await
            .map_err(SessionError::from)?;

        self.strip_datagram_header(datagram)
    }

    /// Validate and remove the session ID a WebTransport datagram carries.
    fn strip_datagram_header(&self, mut datagram: Bytes) -> Result<Bytes, SessionError> {
        let mut cursor = Cursor::new(&datagram);

        if let Some(session_id) = self.session_id {
            // We have to check and strip the session ID from the datagram.
            let actual_id = VarInt::decode(&mut cursor).map_err(|_| SessionError::Unknown)?;
            if actual_id != session_id {
                return Err(SessionError::Unknown);
            }
        }

        // Return the datagram without the session ID.
        Ok(datagram.split_off(cursor.position() as usize))
    }

    /// Sends an application datagram to the remote peer.
    ///
    /// Datagrams are unreliable and may be dropped or delivered out of order.
    /// The data must be smaller than [`max_datagram_size`](Self::max_datagram_size).
    pub fn send_datagram(&self, data: Bytes) -> Result<(), SessionError> {
        if !self.header_datagram.is_empty() {
            // Unfortunately, we need to allocate/copy each datagram because of the quiche API.
            // Pls go +1 if you care: https://github.com/quiche-rs/quiche/issues/1724
            let mut buf = BytesMut::with_capacity(self.header_datagram.len() + data.len());

            // Prepend the datagram with the header indicating the session ID.
            buf.extend_from_slice(&self.header_datagram);
            buf.extend_from_slice(&data);

            self.conn.send_datagram(buf.into())?;
        } else {
            self.conn.send_datagram(data)?;
        }

        Ok(())
    }

    /// Computes the maximum size of datagrams that may be passed to
    /// [`send_datagram`](Self::send_datagram).
    ///
    /// Returns `0` when the peer did not negotiate the QUIC datagram extension
    /// (or the value is otherwise unavailable) — in that case
    /// [`send_datagram`](Self::send_datagram) will drop everything.
    pub fn max_datagram_size(&self) -> usize {
        match self.conn.max_datagram_size() {
            Some(mtu) => mtu.saturating_sub(self.header_datagram.len()),
            None => 0,
        }
    }

    /// Immediately close the connection with an error code and reason.
    ///
    /// The error code is a u32 with WebTransport since it shares the error space with HTTP/3.
    pub fn close(&self, code: u32, reason: &str) {
        let code = if self.session_id.is_some() {
            web_transport_proto::error_to_http3(code)
        } else {
            code.into()
        };

        self.conn.close(code, reason)
    }

    /// Wait until the session is closed, returning the error.
    ///
    /// This method will block until the connection is closed by either the remote peer or locally.
    pub async fn closed(&self) -> SessionError {
        self.conn.closed().await.into()
    }

    /// Create a new session from a raw QUIC connection.
    ///
    /// This is used to pretend like a QUIC connection is a WebTransport session,
    /// making it easier to support WebTransport and raw QUIC simultaneously.
    ///
    /// There is no CONNECT request, so [`Self::request`] returns `None`. The response
    /// is supplied by the caller to carry the negotiated ALPN via [`Self::protocol`].
    pub fn raw(conn: ez::Connection, response: impl Into<ConnectResponse>) -> Self {
        let drop = Arc::new(ConnectionDrop { conn: conn.clone() });
        Self {
            conn,
            drop,
            session_id: None,
            header_uni: Default::default(),
            header_bi: Default::default(),
            header_datagram: Default::default(),
            accept: None,
            settings: None,
            request: None,
            response: response.into(),
            open_uni: OpenUni::Idle,
            open_bi: OpenBi::Idle,
            parked_accept_uni: Parked::default(),
            parked_accept_bi: Parked::default(),
            parked_open_uni: Parked::default(),
            parked_open_bi: Parked::default(),
            parked_recv_datagram: Parked::default(),
            parked_closed: Parked::default(),
        }
    }

    /// Returns the [`ConnectRequest`] if this session was established over HTTP/3,
    /// or `None` for a raw QUIC session.
    pub fn request(&self) -> Option<&ConnectRequest> {
        self.request.as_ref()
    }

    pub fn response(&self) -> &ConnectResponse {
        &self.response
    }

    /// Returns the most recent connection statistics snapshot.
    pub fn stats(&self) -> ez::ConnectionStats {
        self.conn.stats()
    }
}

impl web_transport_trait::Stats for ez::ConnectionStats {
    fn bytes_sent(&self) -> Option<u64> {
        Some(self.bytes_sent)
    }

    fn bytes_received(&self) -> Option<u64> {
        Some(self.bytes_received)
    }

    fn bytes_lost(&self) -> Option<u64> {
        Some(self.bytes_lost)
    }

    fn packets_sent(&self) -> Option<u64> {
        Some(self.packets_sent)
    }

    fn packets_received(&self) -> Option<u64> {
        Some(self.packets_received)
    }

    fn packets_lost(&self) -> Option<u64> {
        Some(self.packets_lost)
    }

    fn rtt(&self) -> Option<std::time::Duration> {
        self.rtt
    }

    fn estimated_send_rate(&self) -> Option<u64> {
        self.send_rate
    }
}

impl web_transport_trait::Session for Connection {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = SessionError;

    async fn accept_uni(&self) -> Result<RecvStream, SessionError> {
        self.accept_uni().await
    }

    async fn accept_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        self.accept_bi().await
    }

    async fn open_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        self.open_bi().await
    }

    async fn open_uni(&self) -> Result<SendStream, SessionError> {
        self.open_uni().await
    }

    fn send_datagram(&self, payload: bytes::Bytes) -> Result<(), Self::Error> {
        self.send_datagram(payload)
    }

    async fn recv_datagram(&self) -> Result<bytes::Bytes, SessionError> {
        self.read_datagram().await
    }

    fn max_datagram_size(&self) -> usize {
        self.max_datagram_size()
    }

    fn protocol(&self) -> Option<&str> {
        self.response().protocol.as_deref()
    }

    fn close(&self, code: u32, reason: &str) {
        self.close(code, reason)
    }

    async fn closed(&self) -> SessionError {
        self.closed().await
    }

    fn stats(&self) -> impl web_transport_trait::Stats {
        self.conn.stats()
    }
}

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
    accept: &Mutex<SessionAccept>,
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

// Type aliases just so clippy doesn't complain about the complexity.
type AcceptUni = dyn Stream<Item = Result<ez::RecvStream, ez::ConnectionError>> + Send;
type AcceptBi =
    dyn Stream<Item = Result<(ez::SendStream, ez::RecvStream), ez::ConnectionError>> + Send;
type PendingUni = dyn Future<Output = Result<(StreamUni, ez::RecvStream), SessionError>> + Send;
type PendingBi =
    dyn Future<Output = Result<Option<(ez::SendStream, ez::RecvStream)>, SessionError>> + Send;

// Logic just for accepting streams, which is annoying because of the stream header.
pub struct SessionAccept {
    session_id: VarInt,

    // We also need to keep a reference to the qpack streams if the endpoint (incorrectly) creates them.
    // Again, this is just so they don't get closed until we drop the session.
    qpack_encoder: Option<ez::RecvStream>,
    qpack_decoder: Option<ez::RecvStream>,

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

    // `Waker::from(waiters.clone())`, cached so `ez` is polled with the same waker every
    // time. That waker outlives every caller, so an accepter that drops its future
    // cannot take the wakeup path with it, and `ez` holds one registration rather than
    // one per caller.
    bi_waker: Waker,
    uni_waker: Waker,
}

impl SessionAccept {
    pub(super) fn new(conn: ez::Connection, session_id: VarInt) -> Self {
        // Create a stream that just outputs new streams, so it's easy to call from poll.
        let accept_uni = Box::pin(futures::stream::unfold(conn.clone(), |conn| async {
            Some((conn.accept_uni().await, conn))
        }));

        let accept_bi = Box::pin(futures::stream::unfold(conn, |conn| async {
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
        // Register before polling, not on the way out: the shared waker can fire from
        // the `ez` driver at any point below, and a wake that lands before the caller
        // is on the list would be lost.
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
                    // Ignore the error, the stream was probably reset early.
                    tracing::warn!(?err, "failed to decode unidirectional stream");
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
        mut recv: ez::RecvStream,
        expected_session: VarInt,
    ) -> Result<(StreamUni, ez::RecvStream), SessionError> {
        // Read the VarInt at the start of the stream.
        let typ = VarInt::read(&mut recv)
            .await
            .map_err(|_| SessionError::Unknown)?;
        let typ = StreamUni(typ);

        if typ == StreamUni::WEBTRANSPORT {
            // Read the session_id and validate it
            let session_id = VarInt::read(&mut recv)
                .await
                .map_err(|_| SessionError::Unknown)?;
            if session_id != expected_session {
                return Err(SessionError::Unknown);
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
                    // Ignore the error, the stream was probably reset early.
                    tracing::warn!(?err, "failed to decode bidirectional stream");
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
        send: ez::SendStream,
        mut recv: ez::RecvStream,
        expected_session: VarInt,
    ) -> Result<Option<(ez::SendStream, ez::RecvStream)>, SessionError> {
        let typ = VarInt::read(&mut recv)
            .await
            .map_err(|_| SessionError::Unknown)?;
        if Frame(typ) != Frame::WEBTRANSPORT {
            tracing::debug!("ignoring unknown bidirectional stream: {typ:?}");
            return Ok(None);
        }

        // Read the session ID and validate it.
        let session_id = VarInt::read(&mut recv)
            .await
            .map_err(|_| SessionError::Unknown)?;
        if session_id != expected_session {
            return Err(SessionError::Unknown);
        }

        Ok(Some((send, recv)))
    }
}

impl web_transport_trait::poll::Session for Connection {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = SessionError;

    fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<RecvStream, SessionError>> {
        match self.accept.clone() {
            // `SessionAccept` decodes stream headers, so it keeps its own state and
            // waiter list; forward to it, holding on to our registration.
            Some(accept) => self
                .parked_accept_uni
                .poll(cx, |waiter| poll_accept_uni_shared(&accept, waiter)),
            None => self.parked_accept_uni.poll(cx, |waiter| {
                let recv = ready!(self.conn.poll_accept_uni(waiter))?;
                Poll::Ready(Ok(RecvStream::new(recv)))
            }),
        }
    }

    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
        match self.accept.clone() {
            Some(accept) => self
                .parked_accept_bi
                .poll(cx, |waiter| poll_accept_bi_shared(&accept, waiter)),
            None => self.parked_accept_bi.poll(cx, |waiter| {
                let (send, recv) = ready!(self.conn.poll_accept_bi(waiter))?;
                Poll::Ready(Ok((SendStream::new(send), RecvStream::new(recv))))
            }),
        }
    }

    fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<SendStream, SessionError>> {
        // One waiter for the whole operation: it can be registered with the connection
        // now and the stream later, and both registrations die together.
        self.parked_open_uni.poll(cx, |waiter| loop {
            match &mut self.open_uni {
                OpenUni::Idle => {
                    let send = ready!(self.conn.poll_open_uni(waiter))?;
                    self.open_uni = OpenUni::Header { send, offset: 0 };
                }
                OpenUni::Header { send, offset } => {
                    while *offset < self.header_uni.len() {
                        let size = ready!(send.poll_write(waiter, &self.header_uni[*offset..]))
                            .map_err(SessionError::Header)?;
                        *offset += size;
                    }

                    // Header written: hand the stream over and go idle.
                    let OpenUni::Header { send, .. } =
                        std::mem::replace(&mut self.open_uni, OpenUni::Idle)
                    else {
                        unreachable!("checked above");
                    };

                    return Poll::Ready(Ok(SendStream::new(send)));
                }
            }
        })
    }

    fn poll_open_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
        self.parked_open_bi.poll(cx, |waiter| loop {
            match &mut self.open_bi {
                OpenBi::Idle => {
                    let (send, recv) = ready!(self.conn.poll_open_bi(waiter))?;
                    self.open_bi = OpenBi::Header {
                        send,
                        recv,
                        offset: 0,
                    };
                }
                OpenBi::Header { send, offset, .. } => {
                    while *offset < self.header_bi.len() {
                        let size = ready!(send.poll_write(waiter, &self.header_bi[*offset..]))
                            .map_err(SessionError::Header)?;
                        *offset += size;
                    }

                    let OpenBi::Header { send, recv, .. } =
                        std::mem::replace(&mut self.open_bi, OpenBi::Idle)
                    else {
                        unreachable!("checked above");
                    };

                    return Poll::Ready(Ok((SendStream::new(send), RecvStream::new(recv))));
                }
            }
        })
    }

    fn poll_send_datagram(
        &mut self,
        _cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), SessionError>> {
        // `ez` queues outbound datagrams into a bounded channel and drops on full,
        // which is the unreliable contract — there is no capacity to wait for, so
        // this never parks.
        let mut buf = BytesMut::with_capacity(self.header_datagram.len() + payload.len());
        buf.extend_from_slice(&self.header_datagram);
        buf.extend_from_slice(payload);

        Poll::Ready(self.conn.send_datagram(buf.into()).map_err(Into::into))
    }

    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, SessionError>> {
        let datagram = ready!(self
            .parked_recv_datagram
            .poll(cx, |waiter| self.conn.poll_read_datagram(waiter)))?;

        Poll::Ready(self.strip_datagram_header(datagram))
    }

    fn max_datagram_size(&self) -> usize {
        Self::max_datagram_size(self)
    }

    fn protocol(&self) -> Option<&str> {
        self.response.protocol.as_deref()
    }

    fn close(&mut self, code: u32, reason: &str) {
        Self::close(self, code, reason);
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<SessionError> {
        self.parked_closed
            .poll(cx, |waiter| self.conn.poll_closed(waiter))
            .map(Into::into)
    }

    #[allow(refining_impl_trait)]
    fn stats(&self) -> ez::ConnectionStats {
        Self::stats(self)
    }
}
