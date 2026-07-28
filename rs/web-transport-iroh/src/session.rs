use std::{
    fmt,
    future::{Future, poll_fn},
    io::Cursor,
    ops::Deref,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use bytes::{Bytes, BytesMut};
use iroh::endpoint::{self, Connection, PathStats};
use n0_future::{
    FuturesUnordered,
    stream::{Stream, StreamExt},
};
use web_transport_proto::{ConnectRequest, ConnectResponse, Frame, Header, StreamUni, VarInt};

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

    // Retained futures for the operations with no native poll form. Each handle
    // owns its own, so `&mut self` is all the exclusion they need.
    ops: web_transport_trait::SessionOps<SendStream, RecvStream, SessionError>,
}

impl Session {
    /// Create a new session from a raw QUIC connection and a URL.
    ///
    /// This is used to pretend like a QUIC connection is a WebTransport session.
    /// It's a hack, but it makes it much easier to support WebTransport and raw QUIC simultaneously.
    pub fn raw(conn: Connection) -> Self {
        Self {
            conn,
            h3: None,
            ops: Default::default(),
        }
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
        let this = Session {
            conn,
            h3: Some(h3),
            ops: Default::default(),
        };
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

    /// Accept a new unidirectional stream. See [`iroh::endpoint::Connection::accept_uni`].
    pub async fn accept_uni(&self) -> Result<RecvStream, SessionError> {
        if let Some(h3) = &self.h3 {
            poll_fn(|cx| h3.accept.lock().unwrap().poll_accept_uni(cx)).await
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
            poll_fn(|cx| h3.accept.lock().unwrap().poll_accept_bi(cx)).await
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
        Self::open_uni_owned(self.conn.clone(), self.header_uni()).await
    }

    /// The header written in front of a new uni stream, empty for raw QUIC.
    fn header_uni(&self) -> Header {
        self.h3.as_ref().map(|h3| h3.header_uni).unwrap_or_default()
    }

    /// The body of [`open_uni`](Self::open_uni), over owned handles.
    ///
    /// `poll_open_uni` has to retain this future between polls, which means it
    /// cannot borrow the session. Taking cheap handles keeps one implementation
    /// rather than two.
    async fn open_uni_owned(conn: Connection, header: Header) -> Result<SendStream, SessionError> {
        let mut send = conn.open_uni().await?;

        if !header.is_empty() {
            write_full_with_max_prio(&mut send, &header).await?;
        }

        Ok(SendStream::new(send))
    }

    /// Open a new bidirectional stream. See [`iroh::endpoint::Connection::open_bi`].
    pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
        Self::open_bi_owned(self.conn.clone(), self.header_bi()).await
    }

    /// The header written in front of a new bidi stream, empty for raw QUIC.
    fn header_bi(&self) -> Header {
        self.h3.as_ref().map(|h3| h3.header_bi).unwrap_or_default()
    }

    /// The body of [`open_bi`](Self::open_bi), over owned handles. See
    /// [`open_uni_owned`](Self::open_uni_owned).
    async fn open_bi_owned(
        conn: Connection,
        header: Header,
    ) -> Result<(SendStream, RecvStream), SessionError> {
        let (mut send, recv) = conn.open_bi().await?;

        if !header.is_empty() {
            write_full_with_max_prio(&mut send, &header).await?;
        }

        Ok((SendStream::new(send), RecvStream::new(recv)))
    }

    /// Asynchronously receives an application datagram from the remote peer.
    ///
    /// This method is used to receive an application datagram sent by the remote
    /// peer over the connection.
    /// It waits for a datagram to become available and returns the received bytes.
    pub async fn read_datagram(&self) -> Result<Bytes, SessionError> {
        Self::read_datagram_owned(self.conn.clone(), self.session_id()).await
    }

    /// The session ID stripped from inbound datagrams, absent for raw QUIC.
    fn session_id(&self) -> Option<VarInt> {
        self.h3.as_ref().map(|h3| h3.session_id)
    }

    /// The body of [`read_datagram`](Self::read_datagram), over owned handles. See
    /// [`open_uni_owned`](Self::open_uni_owned).
    async fn read_datagram_owned(
        conn: Connection,
        session_id: Option<VarInt>,
    ) -> Result<Bytes, SessionError> {
        let mut datagram = conn.read_datagram().await.map_err(SessionError::from)?;

        let datagram = if let Some(session_id) = session_id {
            let mut cursor = Cursor::new(&datagram);

            // We have to check and strip the session ID from the datagram.
            let actual_id =
                VarInt::decode(&mut cursor).map_err(|_| WebTransportError::UnknownSession)?;
            if actual_id != session_id {
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
        Self::closed_owned(self.conn.clone()).await
    }

    /// The body of [`closed`](Self::closed), over an owned handle. See
    /// [`open_uni_owned`](Self::open_uni_owned).
    async fn closed_owned(conn: Connection) -> SessionError {
        conn.closed().await.into()
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
    header_uni: Header,
    header_bi: Header,
    header_datagram: Header,

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
        let header_uni = Header::encode(|w| {
            StreamUni::WEBTRANSPORT.encode(w);
            session_id.encode(w);
        });

        let header_bi = Header::encode(|w| {
            Frame::WEBTRANSPORT.encode(w);
            session_id.encode(w);
        });

        let header_datagram = Header::encode(|w| session_id.encode(w));

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

    // Wakers from concurrent callers of accept_bi / accept_uni.
    // Every clone of the session polls this one struct, and the streams beneath it
    // store only the most recent waker — so a second caller's registration replaces
    // the first's and that caller never wakes. Keep them all and wake them when one
    // caller takes a stream, so the rest retry.
    bi_wakers: Vec<Waker>,
    uni_wakers: Vec<Waker>,
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

        Self {
            session_id,

            qpack_decoder: None,
            qpack_encoder: None,

            accept_uni,
            accept_bi,

            pending_uni: FuturesUnordered::new(),
            pending_bi: FuturesUnordered::new(),
            bi_wakers: Vec::new(),
            uni_wakers: Vec::new(),
        }
    }

    // This is poll-based because we accept and decode streams in parallel.
    // In async land I would use tokio::JoinSet, but that requires a runtime.
    // It's better to use FuturesUnordered instead because it's agnostic.
    /// Remember a caller that is parking, so it can be woken when another takes a
    /// stream and replaces its waker registration.
    fn remember(wakers: &mut Vec<Waker>, cx: &Context<'_>) {
        if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
        }
    }

    /// Wake everyone else so they re-register; whoever polled last owns the
    /// underlying registration.
    fn wake_all(wakers: &mut Vec<Waker>) {
        for waker in wakers.drain(..) {
            waker.wake();
        }
    }

    pub fn poll_accept_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<RecvStream, SessionError>> {
        loop {
            // Accept any new streams.
            if let Poll::Ready(Some(res)) = self.accept_uni.poll_next(cx) {
                // Start decoding the header and add the future to the list of pending streams.
                let recv = match res {
                    Ok(recv) => recv,
                    Err(err) => {
                        Self::wake_all(&mut self.uni_wakers);
                        return Poll::Ready(Err(err.into()));
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
                Poll::Ready(None) | Poll::Pending => {
                    Self::remember(&mut self.uni_wakers, cx);
                    return Poll::Pending;
                }
            };

            // Decide if we keep looping based on the type.
            match typ {
                StreamUni::WEBTRANSPORT => {
                    let recv = RecvStream::new(recv);
                    Self::wake_all(&mut self.uni_wakers);
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

    pub fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
        loop {
            // Accept any new streams.
            if let Poll::Ready(Some(res)) = self.accept_bi.poll_next(cx) {
                // Start decoding the header and add the future to the list of pending streams.
                let (send, recv) = match res {
                    Ok(streams) => streams,
                    Err(err) => {
                        Self::wake_all(&mut self.bi_wakers);
                        return Poll::Ready(Err(err.into()));
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
                Poll::Ready(None) | Poll::Pending => {
                    Self::remember(&mut self.bi_wakers, cx);
                    return Poll::Pending;
                }
            };

            if let Some((send, recv)) = res {
                // Wrap the streams in our own types for correct error codes.
                let send = SendStream::new(send);
                let recv = RecvStream::new(recv);
                Self::wake_all(&mut self.bi_wakers);
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

impl web_transport_trait::PollSession for Session {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = SessionError;

    fn poll_accept_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::RecvStream, Self::Error>> {
        // The WebTransport path is already poll-driven: `H3SessionAccept` decodes
        // stream headers through a `FuturesUnordered` and keeps a waker per
        // concurrent caller, so forward straight to it.
        if let Some(h3) = &self.h3 {
            return h3.accept.lock().unwrap().poll_accept_uni(cx);
        }

        // Raw QUIC has no header to decode, but `accept_uni` registers its
        // `Notified` when the future is built, so the future has to be retained
        // rather than rebuilt each poll.
        let Self { conn, ops, .. } = self;
        ops.accept_uni.poll(cx, || {
            let conn = conn.clone();
            async move {
                conn.accept_uni()
                    .await
                    .map(RecvStream::new)
                    .map_err(Into::into)
            }
        })
    }

    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
        if let Some(h3) = &self.h3 {
            return h3.accept.lock().unwrap().poll_accept_bi(cx);
        }

        let Self { conn, ops, .. } = self;
        ops.accept_bi.poll(cx, || {
            let conn = conn.clone();
            async move {
                conn.accept_bi()
                    .await
                    .map(|(send, recv)| (SendStream::new(send), RecvStream::new(recv)))
                    .map_err(Into::into)
            }
        })
    }

    fn poll_open_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>> {
        // Opening claims stream credit and then writes the WebTransport header, so
        // it is a multi-step routine with no poll form. Retaining it is what stops a
        // `Pending` from abandoning the claim.
        let header = self.header_uni();
        let Self { conn, ops, .. } = self;
        ops.open_uni
            .poll(cx, || Self::open_uni_owned(conn.clone(), header))
    }

    fn poll_open_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
        let header = self.header_bi();
        let Self { conn, ops, .. } = self;
        ops.open_bi
            .poll(cx, || Self::open_bi_owned(conn.clone(), header))
    }

    fn close(&self, code: u32, reason: &str) {
        Self::close(self, code, reason.as_bytes());
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
        let Self { conn, ops, .. } = self;
        ops.closed.poll(cx, || Self::closed_owned(conn.clone()))
    }

    fn send_datagram(&self, data: Bytes) -> Result<(), Self::Error> {
        Self::send_datagram(self, data)
    }

    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
        let session_id = self.session_id();
        let Self { conn, ops, .. } = self;
        ops.recv_datagram
            .poll(cx, || Self::read_datagram_owned(conn.clone(), session_id))
    }

    fn max_datagram_size(&self) -> usize {
        Self::max_datagram_size(self)
    }

    fn protocol(&self) -> Option<&str> {
        match self.h3.as_ref() {
            None => std::str::from_utf8(self.conn.alpn()).ok(),
            Some(h3) => h3.response.protocol.as_deref(),
        }
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

impl web_transport_trait::Session for Session {}

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
