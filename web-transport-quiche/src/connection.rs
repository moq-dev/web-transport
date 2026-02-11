use crate::{ez, h3, ClientError, RecvStream, SendStream, SessionError};

use futures::{ready, stream::FuturesUnordered, Stream, StreamExt};
use web_transport_proto::{ConnectRequest, ConnectResponse, Frame, StreamUni, VarInt};

use std::{
    future::{poll_fn, Future},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
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
    request: ConnectRequest,
    response: ConnectResponse,
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
            request: connect.request.clone(),
            response: connect.response.clone(),
            settings: Some(Arc::new(settings)),
        };

        // Run a background task to check if the connect stream is closed.
        tokio::spawn(this.clone().run_closed(connect));

        tracing::debug!(url = %this.request().url, "WebTransport connection established");

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
            poll_fn(|cx| accept.lock().unwrap().poll_accept_uni(cx)).await
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
            poll_fn(|cx| accept.lock().unwrap().poll_accept_bi(cx)).await
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

    /*
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

        let mut cursor = Cursor::new(&datagram);

        if let Some(session_id) = self.session_id {
            // We have to check and strip the session ID from the datagram.
            let actual_id = VarInt::decode(&mut cursor).map_err(|_| SessionError::Unknown)?;
            if actual_id != session_id {
                return Err(SessionError::Unknown.into());
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
    pub fn max_datagram_size(&self) -> usize {
        let mtu = self
            .conn
            .max_datagram_size()
            .expect("datagram support is required");
        mtu.saturating_sub(self.header_datagram.len())
    }
    */

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

    /// Create a new session from a raw QUIC connection and a URL.
    ///
    /// This is used to pretend like a QUIC connection is a WebTransport session.
    /// It's a hack, but it makes it much easier to support WebTransport and raw QUIC simultaneously.
    pub fn raw(
        conn: ez::Connection,
        request: impl Into<ConnectRequest>,
        response: impl Into<ConnectResponse>,
    ) -> Self {
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
            request: request.into(),
            response: response.into(),
        }
    }

    pub fn request(&self) -> &ConnectRequest {
        &self.request
    }

    pub fn response(&self) -> &ConnectResponse {
        &self.response
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

    fn send_datagram(&self, _payload: bytes::Bytes) -> Result<(), Self::Error> {
        todo!()
    }

    async fn recv_datagram(&self) -> Result<bytes::Bytes, SessionError> {
        todo!()
    }

    fn max_datagram_size(&self) -> usize {
        todo!()
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

        Self {
            session_id,

            qpack_decoder: None,
            qpack_encoder: None,

            accept_uni,
            accept_bi,

            pending_uni: FuturesUnordered::new(),
            pending_bi: FuturesUnordered::new(),
        }
    }

    // This is poll-based because we accept and decode streams in parallel.
    // In async land I would use tokio::JoinSet, but that requires a runtime.
    // It's better to use FuturesUnordered instead because it's agnostic.
    pub fn poll_accept_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<RecvStream, SessionError>> {
        loop {
            // Accept any new streams.
            if let Poll::Ready(Some(res)) = self.accept_uni.poll_next_unpin(cx) {
                // Start decoding the header and add the future to the list of pending streams.
                let recv = res?;
                let pending = Self::decode_uni(recv, self.session_id);
                self.pending_uni.push(Box::pin(pending));

                continue;
            }

            // Poll the list of pending streams.
            let (typ, recv) = match ready!(self.pending_uni.poll_next_unpin(cx)) {
                Some(Ok(res)) => res,
                Some(Err(err)) => {
                    // Ignore the error, the stream was probably reset early.
                    tracing::warn!(?err, "failed to decode unidirectional stream");
                    continue;
                }
                None => return Poll::Pending,
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

    pub fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
        loop {
            // Accept any new streams.
            if let Poll::Ready(Some(res)) = self.accept_bi.poll_next_unpin(cx) {
                // Start decoding the header and add the future to the list of pending streams.
                let (send, recv) = res?;
                let pending = Self::decode_bi(send, recv, self.session_id);
                self.pending_bi.push(Box::pin(pending));

                continue;
            }

            // Poll the list of pending streams.
            let res = match ready!(self.pending_bi.poll_next_unpin(cx)) {
                Some(Ok(res)) => res,
                Some(Err(err)) => {
                    // Ignore the error, the stream was probably reset early.
                    tracing::warn!(?err, "failed to decode bidirectional stream");
                    continue;
                }
                None => return Poll::Pending,
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
