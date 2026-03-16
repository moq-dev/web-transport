use std::{
    collections::{hash_map, HashMap},
    sync::Arc,
};

use crate::config::Config;
use crate::credit::Credit;
use crate::transport::Transport;
use crate::{
    ConnectionClose, Error, Frame, ResetStream, StopSending, Stream, StreamDir, StreamId,
    TransportParams, Version, MAX_FRAME_PAYLOAD,
};
use bytes::{Buf, BufMut, Bytes};
use tokio::sync::{mpsc, watch};
use web_transport_proto::VarInt;
use web_transport_trait as generic;

/// A multiplexed session over a reliable transport.
#[derive(Clone)]
pub struct Session {
    is_server: bool,
    config: Config,

    outbound: mpsc::Sender<Frame>,
    outbound_priority: mpsc::UnboundedSender<Frame>,

    accept_bi: Arc<tokio::sync::Mutex<mpsc::Receiver<(SendStream, RecvStream)>>>,
    accept_uni: Arc<tokio::sync::Mutex<mpsc::Receiver<RecvStream>>>,

    create_uni: mpsc::Sender<(StreamId, SendState)>,
    create_bi: mpsc::Sender<(StreamId, SendState, RecvState)>,

    closed: watch::Sender<Option<Error>>,

    // Flow control: stream count credits (claim_index returns stream sequence number)
    open_bi_credit: Credit,
    open_uni_credit: Credit,

    // Shared connection-level send credit (shared with SendStreams)
    conn_send_credit: Credit,

    // Shared connection-level recv credit (shared with RecvStreams)
    conn_recv_credit: Credit,
}

struct SessionState<T: Transport> {
    transport: T,
    config: Config,
    is_server: bool,

    outbound: (mpsc::Sender<Frame>, mpsc::Receiver<Frame>),
    outbound_priority: (mpsc::UnboundedSender<Frame>, mpsc::UnboundedReceiver<Frame>),

    accept_bi: mpsc::Sender<(SendStream, RecvStream)>,
    accept_uni: mpsc::Sender<RecvStream>,

    create_uni: mpsc::Receiver<(StreamId, SendState)>,
    create_bi: mpsc::Receiver<(StreamId, SendState, RecvState)>,

    send_streams: HashMap<StreamId, SendState>,
    recv_streams: HashMap<StreamId, RecvState>,

    closed: watch::Sender<Option<Error>>,

    // Flow control state
    conn_send_credit: Credit,
    conn_recv_credit: Credit,
    our_params: TransportParams,
    peer_params: TransportParams,
    params_received: bool,

    // Stream count tracking
    open_bi_credit: Credit,
    open_uni_credit: Credit,
    recv_bi_credit: Credit,
    recv_uni_credit: Credit,
}

impl<T: Transport> SessionState<T> {
    // WARNING: Cancellation safety issue!
    //
    // self.transport.recv_frame() is NOT cancellation-safe for StreamTransport
    // (TCP/TLS) because it performs multi-step reads (read_u8, read_exact, etc.).
    // If another select! branch fires while recv_frame is mid-parse, the future
    // is dropped and restarted on the next iteration, desynchronizing the stream.
    //
    // This is mitigated by `biased` (recv_frame is polled first), but not fully
    // fixed — other branches can still win when recv_frame is pending on I/O.
    //
    // WsTransport is unaffected since each WebSocket message is atomic.
    //
    // A proper fix requires splitting Transport into separate read/write halves
    // or moving recv_frame into a dedicated task that never gets cancelled.
    async fn run(&mut self) -> Result<(), Error> {
        // QMux requires TRANSPORT_PARAMETERS as the first frame on the connection.
        if self.config.version == Version::QMux00 {
            self.send_transport_parameters().await?;
        }

        let mut closed = self.closed.subscribe();

        loop {
            tokio::select! {
                biased;
                result = self.transport.recv() => {
                    let data = result?;
                    if let Some(frame) = Frame::decode(data, self.config.version)? {
                        self.recv_frame(frame).await?;
                    }
                }
                Some((id, send)) = self.create_uni.recv() => {
                    // Apply peer's stream credit if transport params already received
                    if self.params_received {
                        if let Some(credit) = &send.stream_credit {
                            credit.increase_max(self.peer_params.initial_max_stream_data_uni).ok();
                        }
                    }
                    self.send_streams.insert(id, send);
                }
                Some((id, send, recv)) = self.create_bi.recv() => {
                    if self.params_received {
                        if let Some(credit) = &send.stream_credit {
                            credit.increase_max(self.peer_params.initial_max_stream_data_bidi_remote).ok();
                        }
                    }
                    self.send_streams.insert(id, send);
                    self.recv_streams.insert(id, recv);
                }
                frame = self.outbound_priority.1.recv() => {
                    match frame {
                        Some(frame) => self.send_frame(frame).await?,
                        None => return Err(Error::Closed),
                    };
                }
                frame = self.outbound.1.recv() => {
                    match frame {
                        Some(frame) => self.send_frame(frame).await?,
                        None => return Err(Error::Closed),
                    };
                }
                _ = async { closed.wait_for(|err| err.is_some()).await.ok(); } => {
                    return Err(closed.borrow().clone().unwrap_or(Error::Closed))
                }
            }
        }
    }

    /// Send a QX_TRANSPORT_PARAMETERS frame with our defaults.
    async fn send_transport_parameters(&mut self) -> Result<(), Error> {
        let frame = Frame::TransportParameters(self.our_params.clone());
        self.transport
            .send(frame.encode(self.config.version)?)
            .await
    }

    async fn send_frame(&mut self, frame: Frame) -> Result<(), Error> {
        // Update our state first.
        match &frame {
            Frame::ResetStream(reset) => {
                self.send_streams.remove(&reset.id);
            }
            Frame::Stream(stream) if stream.fin => {
                self.send_streams.remove(&stream.id);
            }
            Frame::StopSending(stop) => {
                self.recv_streams.remove(&stop.id);
            }
            _ => {}
        };

        self.transport
            .send(frame.encode(self.config.version)?)
            .await
    }

    async fn recv_frame(&mut self, frame: Frame) -> Result<(), Error> {
        match frame {
            Frame::TransportParameters(params) => {
                self.recv_transport_parameters(params)?;
            }
            Frame::Stream(stream) => {
                if stream.data.len() > MAX_FRAME_PAYLOAD {
                    return Err(Error::FrameTooLarge);
                }

                if !stream.id.can_recv(self.is_server) {
                    return Err(Error::InvalidStreamId);
                }

                // Validate receive-side flow control
                let data_len = stream.data.len() as u64;
                if data_len > 0 {
                    // Connection-level check
                    if !self.conn_recv_credit.receive(data_len) {
                        return Err(Error::FlowControlError);
                    }

                    // Stream-level check (for existing streams)
                    if let Some(recv) = self.recv_streams.get(&stream.id) {
                        if !recv.recv_credit.receive(data_len) {
                            return Err(Error::FlowControlError);
                        }
                    }
                    // For new streams, we check after creation below
                }

                match self.recv_streams.entry(stream.id) {
                    hash_map::Entry::Vacant(e) => {
                        if self.is_server == stream.id.server_initiated() {
                            // Already closed, ignore it.
                            return Ok(());
                        }

                        // Validate stream count limits (QMux only)
                        // Per QUIC RFC 9000 §4.6, the limit applies to the stream index,
                        // not the count of seen streams. A peer opening stream index N
                        // implicitly opens all streams 0..N.
                        if self.config.version == Version::QMux00 {
                            let credit = match stream.id.dir() {
                                StreamDir::Bi => &self.recv_bi_credit,
                                StreamDir::Uni => &self.recv_uni_credit,
                            };
                            if !credit.receive_up_to(stream.id.index() + 1) {
                                return Err(Error::StreamLimitExceeded);
                            }
                        }

                        let (tx, rx) = mpsc::unbounded_channel();
                        let (tx2, rx2) = mpsc::unbounded_channel();

                        // Determine initial stream recv window
                        let recv_window = if self.config.version == Version::QMux00 {
                            match stream.id.dir() {
                                StreamDir::Bi => {
                                    self.our_params.initial_max_stream_data_bidi_remote
                                }
                                StreamDir::Uni => self.our_params.initial_max_stream_data_uni,
                            }
                        } else {
                            u64::MAX
                        };

                        let recv_credit = Credit::new(recv_window);

                        // Validate stream-level for the first frame on new stream
                        if data_len > 0 && !recv_credit.receive(data_len) {
                            return Err(Error::FlowControlError);
                        }

                        let recv_backend = RecvState {
                            inbound_data: tx,
                            inbound_reset: tx2,
                            recv_credit: recv_credit.clone(),
                        };

                        let recv_streams_credit = if self.config.version == Version::QMux00 {
                            Some(match stream.id.dir() {
                                StreamDir::Bi => self.recv_bi_credit.clone(),
                                StreamDir::Uni => self.recv_uni_credit.clone(),
                            })
                        } else {
                            None
                        };

                        let recv_frontend = RecvStream {
                            id: stream.id,
                            inbound_data: rx,
                            inbound_reset: rx2,
                            outbound_priority: self.outbound_priority.0.clone(),
                            buffer: Bytes::new(),
                            closed: None,
                            fin: false,
                            recv_credit,
                            conn_recv_credit: self.conn_recv_credit.clone(),
                            version: self.config.version,
                            recv_streams_credit,
                        };

                        match stream.id.dir() {
                            StreamDir::Uni => {
                                self.accept_uni
                                    .send(recv_frontend)
                                    .await
                                    .map_err(|_| Error::Closed)?;
                            }
                            StreamDir::Bi => {
                                let (tx, rx) = mpsc::unbounded_channel();
                                let send_backend = SendState {
                                    inbound_stopped: tx,
                                    stream_credit: if self.config.version == Version::QMux00 {
                                        // Peer opened this bidi stream, so our send limit
                                        // is their bidi_local (they are local to this stream)
                                        Some(Credit::new(
                                            self.peer_params.initial_max_stream_data_bidi_local,
                                        ))
                                    } else {
                                        None
                                    },
                                };

                                let send_frontend = SendStream {
                                    id: stream.id,
                                    outbound: self.outbound.0.clone(),
                                    outbound_priority: self.outbound_priority.0.clone(),
                                    inbound_stopped: rx,
                                    offset: 0,
                                    closed: None,
                                    fin: false,
                                    stream_credit: send_backend.stream_credit.clone(),
                                    conn_credit: if self.config.version == Version::QMux00 {
                                        Some(self.conn_send_credit.clone())
                                    } else {
                                        None
                                    },
                                };

                                self.send_streams.insert(stream.id, send_backend);
                                self.accept_bi
                                    .send((send_frontend, recv_frontend))
                                    .await
                                    .map_err(|_| Error::Closed)?;
                            }
                        };

                        let fin = stream.fin;
                        recv_backend.inbound_data.send(stream).ok();

                        if !fin {
                            e.insert(recv_backend);
                        }
                    }
                    hash_map::Entry::Occupied(mut e) => {
                        let fin = stream.fin;
                        e.get_mut().inbound_data.send(stream).ok();
                        if fin {
                            e.remove();
                        }
                    }
                };
            }
            Frame::ResetStream(reset) => {
                if !reset.id.can_recv(self.is_server) {
                    return Err(Error::InvalidStreamId);
                }

                if let hash_map::Entry::Occupied(mut e) = self.recv_streams.entry(reset.id) {
                    e.get_mut().inbound_reset.send(reset).ok();
                    e.remove();
                }
            }
            Frame::StopSending(stop) => {
                if !stop.id.can_send(self.is_server) {
                    return Err(Error::InvalidStreamId);
                }

                if let Some(stream) = self.send_streams.get_mut(&stop.id) {
                    stream.inbound_stopped.send(stop).ok();
                }
            }
            Frame::ConnectionClose(close) => {
                self.closed
                    .send(Some(Error::ConnectionClosed {
                        code: close.code,
                        reason: close.reason,
                    }))
                    .ok();
            }
            // Flow control frames
            Frame::MaxData(max) => {
                self.conn_send_credit.increase_max(max)?;
            }
            Frame::MaxStreamData { id, max } => {
                if let Some(send) = self.send_streams.get(&id) {
                    if let Some(credit) = &send.stream_credit {
                        credit.increase_max(max)?;
                    }
                }
            }
            Frame::MaxStreamsBidi(max) => {
                self.open_bi_credit.increase_max(max)?;
            }
            Frame::MaxStreamsUni(max) => {
                self.open_uni_credit.increase_max(max)?;
            }
            // Informational frames — peer is telling us they're blocked.
            // We don't need to act on these since we auto-tune windows.
            Frame::DataBlocked(_)
            | Frame::StreamDataBlocked { .. }
            | Frame::StreamsBlockedBidi(_)
            | Frame::StreamsBlockedUni(_) => {}
        }

        Ok(())
    }

    fn recv_transport_parameters(&mut self, params: TransportParams) -> Result<(), Error> {
        if self.params_received {
            // Duplicate transport parameters
            return Err(Error::FlowControlError);
        }
        self.params_received = true;

        // Set connection-level send credit from peer's initial_max_data
        self.conn_send_credit
            .increase_max(params.initial_max_data)
            .ok();

        // Set stream count limits from peer's params
        self.open_bi_credit
            .increase_max(params.initial_max_streams_bidi)
            .ok();
        self.open_uni_credit
            .increase_max(params.initial_max_streams_uni)
            .ok();

        // Update per-stream send credits for already-opened streams
        for (id, send) in &self.send_streams {
            if let Some(credit) = &send.stream_credit {
                let initial = match id.dir() {
                    StreamDir::Bi => {
                        if id.server_initiated() == self.is_server {
                            // We initiated this stream — peer's bidi_remote applies
                            params.initial_max_stream_data_bidi_remote
                        } else {
                            // Peer initiated this stream — peer's bidi_local applies
                            params.initial_max_stream_data_bidi_local
                        }
                    }
                    StreamDir::Uni => params.initial_max_stream_data_uni,
                };
                credit.increase_max(initial).ok();
            }
        }

        self.peer_params = params;

        Ok(())
    }
}

impl Session {
    /// Create a client-side session over the given transport.
    pub fn connect<T: Transport>(transport: T, config: Config) -> Self {
        Self::new(transport, false, config)
    }

    /// Create a server-side session over the given transport.
    pub fn accept<T: Transport>(transport: T, config: Config) -> Self {
        Self::new(transport, true, config)
    }

    fn new<T: Transport>(transport: T, is_server: bool, config: Config) -> Self {
        let version = config.version;
        let our_params = config.to_transport_params();

        let (accept_bi_tx, accept_bi_rx) = mpsc::channel(1024);
        let (accept_uni_tx, accept_uni_rx) = mpsc::channel(1024);

        let (create_uni_tx, create_uni_rx) = mpsc::channel(8);
        let (create_bi_tx, create_bi_rx) = mpsc::channel(8);

        let (outbound_tx, outbound_rx) = mpsc::channel(8);
        let (outbound_priority_tx, outbound_priority_rx) = mpsc::unbounded_channel();

        let closed = watch::Sender::new(None);

        let open_bi_credit = Credit::new(if version == Version::QMux00 {
            0
        } else {
            u64::MAX
        });
        let open_uni_credit = Credit::new(if version == Version::QMux00 {
            0
        } else {
            u64::MAX
        });

        let conn_send_credit = Credit::new(if version == Version::QMux00 {
            0
        } else {
            u64::MAX
        });

        let conn_recv_credit = Credit::new(if version == Version::QMux00 {
            our_params.initial_max_data
        } else {
            u64::MAX
        });

        // Stream count credits for incoming streams
        let recv_bi_credit = Credit::new(if version == Version::QMux00 {
            config.max_streams_bidi
        } else {
            u64::MAX
        });
        let recv_uni_credit = Credit::new(if version == Version::QMux00 {
            config.max_streams_uni
        } else {
            u64::MAX
        });

        let mut backend = SessionState {
            transport,
            config: config.clone(),
            outbound: (outbound_tx.clone(), outbound_rx),
            outbound_priority: (outbound_priority_tx.clone(), outbound_priority_rx),
            accept_bi: accept_bi_tx,
            accept_uni: accept_uni_tx,
            create_uni: create_uni_rx,
            create_bi: create_bi_rx,
            is_server,
            send_streams: HashMap::new(),
            recv_streams: HashMap::new(),
            closed: closed.clone(),
            conn_send_credit: conn_send_credit.clone(),
            conn_recv_credit: conn_recv_credit.clone(),
            our_params: our_params.clone(),
            peer_params: TransportParams::default(),
            params_received: false,
            open_bi_credit: open_bi_credit.clone(),
            open_uni_credit: open_uni_credit.clone(),
            recv_bi_credit: recv_bi_credit.clone(),
            recv_uni_credit: recv_uni_credit.clone(),
        };
        tokio::spawn(async move {
            let err = backend.run().await.err().unwrap_or(Error::Closed);
            // Close all credits so blocked claim()/claim_index() calls unblock
            backend.open_bi_credit.close();
            backend.open_uni_credit.close();
            backend.conn_send_credit.close();
            backend.conn_recv_credit.close();
            for send in backend.send_streams.values() {
                if let Some(credit) = &send.stream_credit {
                    credit.close();
                }
            }
            backend.closed.send(Some(err)).ok();
        });

        Session {
            is_server,
            config,
            outbound: outbound_tx,
            outbound_priority: outbound_priority_tx,
            accept_bi: Arc::new(tokio::sync::Mutex::new(accept_bi_rx)),
            accept_uni: Arc::new(tokio::sync::Mutex::new(accept_uni_rx)),
            create_uni: create_uni_tx,
            create_bi: create_bi_tx,
            closed,
            open_bi_credit,
            open_uni_credit,
            conn_send_credit,
            conn_recv_credit,
        }
    }
}

impl generic::Session for Session {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = Error;

    async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
        self.accept_uni
            .lock()
            .await
            .recv()
            .await
            .ok_or(Error::Closed)
    }

    async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
        self.accept_bi
            .lock()
            .await
            .recv()
            .await
            .ok_or(Error::Closed)
    }

    async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
        // Wait for stream count credit (blocks until peer's MAX_STREAMS allows it)
        let index = self.open_uni_credit.claim_index().await?;
        let id = StreamId::new(index, StreamDir::Uni, self.is_server);

        let (tx, rx) = mpsc::unbounded_channel();

        let stream_credit = if self.config.version == Version::QMux00 {
            // For uni streams we initiate, peer's uni limit applies
            Some(Credit::new(0)) // Will be set when peer params arrive
        } else {
            None
        };

        let send_backend = SendState {
            inbound_stopped: tx,
            stream_credit: stream_credit.clone(),
        };
        let send_frontend = SendStream {
            id,
            outbound: self.outbound.clone(),
            outbound_priority: self.outbound_priority.clone(),
            inbound_stopped: rx,
            offset: 0,
            closed: None,
            fin: false,
            stream_credit,
            conn_credit: if self.config.version == Version::QMux00 {
                Some(self.conn_send_credit.clone())
            } else {
                None
            },
        };

        self.create_uni
            .send((id, send_backend))
            .await
            .map_err(|_| Error::Closed)?;

        Ok(send_frontend)
    }

    async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
        // Wait for stream count credit (blocks until peer's MAX_STREAMS allows it)
        let index = self.open_bi_credit.claim_index().await?;
        let id = StreamId::new(index, StreamDir::Bi, self.is_server);

        let (tx, rx) = mpsc::unbounded_channel();
        let (tx2, rx2) = mpsc::unbounded_channel();

        let stream_credit = if self.config.version == Version::QMux00 {
            // For bidi streams we initiate, peer's bidi_remote applies to our sends
            Some(Credit::new(0)) // Will be set when peer params arrive
        } else {
            None
        };

        let send_backend = SendState {
            inbound_stopped: tx,
            stream_credit: stream_credit.clone(),
        };
        let send_frontend = SendStream {
            id,
            outbound: self.outbound.clone(),
            outbound_priority: self.outbound_priority.clone(),
            inbound_stopped: rx,
            offset: 0,
            closed: None,
            fin: false,
            stream_credit,
            conn_credit: if self.config.version == Version::QMux00 {
                Some(self.conn_send_credit.clone())
            } else {
                None
            },
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let recv_window = if self.config.version == Version::QMux00 {
            self.config.max_stream_data_bidi_local
        } else {
            u64::MAX
        };
        let recv_credit = Credit::new(recv_window);
        let recv_backend = RecvState {
            inbound_data: tx,
            inbound_reset: tx2,
            recv_credit: recv_credit.clone(),
        };
        let recv_frontend = RecvStream {
            id,
            inbound_data: rx,
            inbound_reset: rx2,
            outbound_priority: self.outbound_priority.clone(),
            buffer: Bytes::new(),
            closed: None,
            fin: false,
            recv_credit,
            conn_recv_credit: self.conn_recv_credit.clone(),
            version: self.config.version,
            recv_streams_credit: None, // We initiated this stream, no stream count tracking
        };

        self.create_bi
            .send((id, send_backend, recv_backend))
            .await
            .map_err(|_| Error::Closed)?;

        Ok((send_frontend, recv_frontend))
    }

    fn close(&self, code: u32, reason: &str) {
        let frame = ConnectionClose {
            code: VarInt::from(code),
            reason: reason.to_string(),
        };
        let _ = self.outbound_priority.send(frame.into());

        self.closed
            .send(Some(Error::ConnectionClosed {
                code: VarInt::from(code),
                reason: reason.to_string(),
            }))
            .ok();
    }

    async fn closed(&self) -> Self::Error {
        let mut closed = self.closed.subscribe();
        closed
            .wait_for(|err| err.is_some())
            .await
            .map(|e| e.clone().unwrap_or(Error::Closed))
            .unwrap_or(Error::Closed)
    }

    fn send_datagram(&self, _payload: Bytes) -> Result<(), Self::Error> {
        Err(Error::DatagramsUnsupported)
    }

    fn max_datagram_size(&self) -> usize {
        0
    }

    async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
        Err(Error::DatagramsUnsupported)
    }

    fn protocol(&self) -> Option<&str> {
        self.config.protocol.as_deref()
    }
}

struct SendState {
    inbound_stopped: mpsc::UnboundedSender<StopSending>,
    stream_credit: Option<Credit>,
}

/// The send half of a multiplexed stream.
pub struct SendStream {
    id: StreamId,

    outbound: mpsc::Sender<Frame>,                   // STREAM
    outbound_priority: mpsc::UnboundedSender<Frame>, // RESET_STREAM
    inbound_stopped: mpsc::UnboundedReceiver<StopSending>,

    offset: u64,
    closed: Option<Error>,
    fin: bool,

    // Flow control (None for WebTransport version)
    stream_credit: Option<Credit>,
    conn_credit: Option<Credit>,
}

impl SendStream {
    fn recv_stop(&mut self, code: VarInt) -> Error {
        if let Some(error) = &self.closed {
            return error.clone();
        }

        let frame = ResetStream {
            id: self.id,
            code,
            final_size: self.offset,
        };

        let error = Error::StreamStop(code);

        self.outbound_priority.send(frame.into()).ok();
        self.closed = Some(error.clone());

        error
    }

    /// Release previously claimed credit (on send failure).
    fn release_credit(&self, amount: u64) {
        if let Some(s) = &self.stream_credit {
            s.release(amount);
        }
        if let Some(c) = &self.conn_credit {
            c.release(amount);
        }
    }

    /// Try to claim flow control credit for sending `desired` bytes.
    /// Returns the number of bytes we're allowed to send.
    async fn claim_credit(&mut self, desired: u64) -> Result<u64, Error> {
        let (stream_credit, conn_credit) = match (&self.stream_credit, &self.conn_credit) {
            (Some(s), Some(c)) => (s, c),
            _ => return Ok(desired), // No flow control
        };

        loop {
            // 1. Try to claim stream credit
            let stream_claimed = stream_credit.try_claim(desired);
            if stream_claimed == 0 {
                // Wait for stream credit or stop_sending
                tokio::select! {
                    result = stream_credit.claim(desired) => {
                        let claimed = result?;
                        // Release and retry the full loop to coordinate with conn credit
                        stream_credit.release(claimed);
                    }
                    Some(stop) = self.inbound_stopped.recv() => {
                        return Err(self.recv_stop(stop.code));
                    }
                }
                continue;
            }

            // 2. Try to claim connection credit (may get less than stream_claimed)
            let conn_claimed = conn_credit.try_claim(stream_claimed);
            if conn_claimed == 0 {
                stream_credit.release(stream_claimed);
                tokio::select! {
                    result = conn_credit.claim(1) => {
                        let claimed = result?;
                        conn_credit.release(claimed); // Release, retry full loop
                    }
                    Some(stop) = self.inbound_stopped.recv() => {
                        return Err(self.recv_stop(stop.code));
                    }
                }
                continue;
            }

            // Return excess stream credit if connection had less
            if conn_claimed < stream_claimed {
                stream_credit.release(stream_claimed - conn_claimed);
            }

            return Ok(conn_claimed);
        }
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        if !self.fin && self.closed.is_none() {
            generic::SendStream::reset(self, 0);
        }
    }
}

impl generic::SendStream for SendStream {
    type Error = Error;

    async fn write(&mut self, mut buf: &[u8]) -> Result<usize, Self::Error> {
        let size = buf.len();
        let b = &mut buf;
        self.write_buf(b).await?;
        Ok(size - b.len())
    }

    async fn write_buf<B: Buf + Send>(&mut self, buf: &mut B) -> Result<usize, Self::Error> {
        if let Some(error) = &self.closed {
            return Err(error.clone());
        }

        if self.fin {
            return Err(Error::StreamClosed);
        }

        let mut total = 0;

        while buf.has_remaining() {
            let chunk_len = buf.chunk().len().min(MAX_FRAME_PAYLOAD) as u64;

            // Claim flow control credit
            let allowed = self.claim_credit(chunk_len).await?;
            let to_send = allowed as usize;

            let frame = Stream {
                id: self.id,
                data: buf.copy_to_bytes(to_send),
                fin: false,
            };

            tokio::select! {
                result = self.outbound.send(frame.into()) => {
                    if result.is_err() {
                        // Release credit since data was never sent
                        self.release_credit(to_send as u64);
                        return Err(Error::Closed);
                    }
                    self.offset += to_send as u64;
                    total += to_send;
                }
                Some(stop) = self.inbound_stopped.recv() => {
                    // Release credit since data was never sent
                    self.release_credit(to_send as u64);
                    return Err(self.recv_stop(stop.code));
                }
            }
        }

        Ok(total)
    }

    /// No-op: QMux does not support stream prioritization.
    fn set_priority(&mut self, _priority: u8) {}

    fn reset(&mut self, code: u32) {
        if self.fin || self.closed.is_some() {
            return;
        }

        let code = VarInt::from(code);
        let frame = ResetStream {
            id: self.id,
            code,
            final_size: self.offset,
        };

        self.outbound_priority.send(frame.into()).ok();
        self.closed = Some(Error::StreamReset(code));
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        if let Some(error) = &self.closed {
            return Err(error.clone());
        }

        let frame = Stream {
            id: self.id,
            data: Bytes::new(),
            fin: true,
        };

        if let Err(e) = self.outbound.try_send(frame.into()) {
            let outbound = self.outbound.clone();
            tokio::spawn(async move {
                outbound.send(e.into_inner()).await.ok();
            });
        }

        self.fin = true;

        Ok(())
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        if let Some(error) = &self.closed {
            return Err(error.clone());
        }

        match self.inbound_stopped.recv().await {
            Some(stop) => Err(self.recv_stop(stop.code)),
            None => Err(Error::Closed),
        }
    }
}

pub(crate) struct RecvState {
    inbound_data: mpsc::UnboundedSender<Stream>,
    inbound_reset: mpsc::UnboundedSender<ResetStream>,
    recv_credit: Credit,
}

/// The receive half of a multiplexed stream.
pub struct RecvStream {
    id: StreamId,
    version: Version,

    outbound_priority: mpsc::UnboundedSender<Frame>, // STOP_SENDING
    inbound_data: mpsc::UnboundedReceiver<Stream>,
    inbound_reset: mpsc::UnboundedReceiver<ResetStream>,

    buffer: Bytes,

    closed: Option<Error>,
    fin: bool,

    // Flow control: per-stream and connection-level recv credit
    recv_credit: Credit,
    conn_recv_credit: Credit,

    // Stream count credit — consume(1) on drop triggers MAX_STREAMS
    recv_streams_credit: Option<Credit>,
}

impl RecvStream {
    fn recv_reset(&mut self, code: VarInt) -> Error {
        if let Some(error) = &self.closed {
            return error.clone();
        }

        self.closed = Some(Error::StreamReset(code));
        Error::StreamReset(code)
    }

    /// Report consumed bytes to flow control, sending window updates as needed.
    fn report_consumed(&self, len: u64) {
        if self.version != Version::QMux00 {
            return;
        }

        // Per-stream window update
        if let Some(new_max) = self.recv_credit.consume(len) {
            let frame = Frame::MaxStreamData {
                id: self.id,
                max: new_max,
            };
            self.outbound_priority.send(frame).ok();
        }

        // Connection-level window update
        if let Some(new_max) = self.conn_recv_credit.consume(len) {
            let frame = Frame::MaxData(new_max);
            self.outbound_priority.send(frame).ok();
        }
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        if !self.fin && self.closed.is_none() {
            generic::RecvStream::stop(self, 0);
        }

        // Replenish stream count when this recv half is done
        if let Some(credit) = &self.recv_streams_credit {
            if let Some(new_max) = credit.consume(1) {
                let frame = match self.id.dir() {
                    StreamDir::Bi => Frame::MaxStreamsBidi(new_max),
                    StreamDir::Uni => Frame::MaxStreamsUni(new_max),
                };
                self.outbound_priority.send(frame).ok();
            }
        }
    }
}

impl generic::RecvStream for RecvStream {
    type Error = Error;

    async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Self::Error> {
        loop {
            if !self.buffer.is_empty() {
                let to_read = max.min(self.buffer.len());
                let data = self.buffer.split_to(to_read);

                // Report consumed bytes and send window updates if needed
                self.report_consumed(to_read as u64);

                return Ok(Some(data));
            }

            if self.fin {
                return Ok(None);
            }

            if let Some(error) = &self.closed {
                return Err(error.clone());
            }

            tokio::select! {
                Some(stream) = self.inbound_data.recv() => {
                    assert_eq!(stream.id, self.id);
                    self.fin = stream.fin;
                    self.buffer = stream.data;
                }
                Some(reset) = self.inbound_reset.recv() => {
                    return Err(self.recv_reset(reset.code));
                }
                else => return Err(Error::Closed),
            }
        }
    }

    async fn read_buf<B: BufMut + Send>(
        &mut self,
        buf: &mut B,
    ) -> Result<Option<usize>, Self::Error> {
        if !self.buffer.is_empty() {
            let to_read = buf.remaining_mut().min(self.buffer.len());
            buf.put(self.buffer.split_to(to_read));

            self.report_consumed(to_read as u64);

            return Ok(Some(to_read));
        }

        Ok(match self.read_chunk(buf.remaining_mut()).await? {
            Some(data) if !data.is_empty() => {
                let size = data.len();
                buf.put(data);
                Some(size)
            }
            _ => None,
        })
    }

    async fn read(&mut self, mut buf: &mut [u8]) -> Result<Option<usize>, Self::Error> {
        self.read_buf(&mut buf).await
    }

    fn stop(&mut self, code: u32) {
        let code = VarInt::from(code);
        let frame = StopSending { id: self.id, code };

        self.outbound_priority.send(frame.into()).ok();
        self.closed = Some(Error::StreamStop(code));
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        if let Some(error) = &self.closed {
            return Err(error.clone());
        }

        loop {
            if self.fin {
                return Ok(());
            }

            tokio::select! {
                Some(reset) = self.inbound_reset.recv() => {
                    return Err(self.recv_reset(reset.code));
                }
                Some(stream) = self.inbound_data.recv() => {
                    assert_eq!(stream.id, self.id);
                    self.buffer = stream.data;
                    self.fin = stream.fin;
                }
                else => {
                    return Err(Error::Closed);
                }
            }
        }
    }
}
