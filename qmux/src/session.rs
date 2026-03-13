use std::{
    collections::{hash_map, HashMap},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use crate::transport::Transport;
use crate::{
    ConnectionClose, Error, Frame, ResetStream, StopSending, Stream, StreamDir, StreamId, Version,
    MAX_FRAME_PAYLOAD,
};
use bytes::{Buf, BufMut, Bytes};
use tokio::sync::{mpsc, watch};
use web_transport_proto::VarInt;
use web_transport_trait as generic;

/// A multiplexed session over a reliable transport.
#[derive(Clone)]
pub struct Session {
    is_server: bool,

    outbound: mpsc::Sender<Frame>,
    outbound_priority: mpsc::UnboundedSender<Frame>,

    accept_bi: Arc<tokio::sync::Mutex<mpsc::Receiver<(SendStream, RecvStream)>>>,
    accept_uni: Arc<tokio::sync::Mutex<mpsc::Receiver<RecvStream>>>,

    create_uni: mpsc::Sender<(StreamId, SendState)>,
    create_bi: mpsc::Sender<(StreamId, SendState, RecvState)>,

    create_uni_id: Arc<AtomicU64>,
    create_bi_id: Arc<AtomicU64>,

    closed: watch::Sender<Option<Error>>,

    /// The negotiated application-level subprotocol, if any.
    protocol: Option<String>,
}

struct SessionState<T: Transport> {
    transport: T,
    version: Version,
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
        if self.version == Version::QMux00 {
            self.send_transport_parameters().await?;
        }

        let mut closed = self.closed.subscribe();

        loop {
            tokio::select! {
                biased;
                result = self.transport.recv() => {
                    let data = result?;
                    if let Some(frame) = Frame::decode(data, self.version)? {
                        self.recv_frame(frame).await?;
                    }
                }
                Some((id, send)) = self.create_uni.recv() => {
                    self.send_streams.insert(id, send);
                }
                Some((id, send, recv)) = self.create_bi.recv() => {
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

    /// Send a QX_TRANSPORT_PARAMETERS frame with default (empty) parameters.
    async fn send_transport_parameters(&mut self) -> Result<(), Error> {
        use bytes::BytesMut;

        let mut buf = BytesMut::new();
        // Frame type: 0x3f5153300d0a0d0a (QX_TRANSPORT_PARAMETERS)
        VarInt::try_from(0x3f5153300d0a0d0au64)
            .unwrap()
            .encode(&mut buf);
        // Length: 0 (no transport parameters)
        VarInt::from(0u32).encode(&mut buf);

        self.transport.send(buf.freeze()).await
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

        self.transport.send(frame.encode(self.version)).await
    }

    async fn recv_frame(&mut self, frame: Frame) -> Result<(), Error> {
        match frame {
            Frame::Stream(stream) => {
                if stream.data.len() > MAX_FRAME_PAYLOAD {
                    return Err(Error::FrameTooLarge);
                }

                if !stream.id.can_recv(self.is_server) {
                    return Err(Error::InvalidStreamId);
                }

                match self.recv_streams.entry(stream.id) {
                    hash_map::Entry::Vacant(e) => {
                        if self.is_server == stream.id.server_initiated() {
                            // Already closed, ignore it.
                            return Ok(());
                        }

                        let (tx, rx) = mpsc::unbounded_channel();
                        let (tx2, rx2) = mpsc::unbounded_channel();

                        let recv_backend = RecvState {
                            inbound_data: tx,
                            inbound_reset: tx2,
                        };

                        let recv_frontend = RecvStream {
                            id: stream.id,
                            inbound_data: rx,
                            inbound_reset: rx2,
                            outbound_priority: self.outbound_priority.0.clone(),
                            buffer: Bytes::new(),
                            closed: None,
                            fin: false,
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
                                };

                                let send_frontend = SendStream {
                                    id: stream.id,
                                    outbound: self.outbound.0.clone(),
                                    outbound_priority: self.outbound_priority.0.clone(),
                                    inbound_stopped: rx,
                                    offset: 0,
                                    closed: None,
                                    fin: false,
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
        }

        Ok(())
    }
}

impl Session {
    /// Create a client-side session over the given transport.
    pub fn connect<T: Transport>(transport: T, version: Version, protocol: Option<String>) -> Self {
        Self::new(transport, version, false, protocol)
    }

    /// Create a server-side session over the given transport.
    pub fn accept<T: Transport>(transport: T, version: Version, protocol: Option<String>) -> Self {
        Self::new(transport, version, true, protocol)
    }

    fn new<T: Transport>(
        transport: T,
        version: Version,
        is_server: bool,
        protocol: Option<String>,
    ) -> Self {
        let (accept_bi_tx, accept_bi_rx) = mpsc::channel(1024);
        let (accept_uni_tx, accept_uni_rx) = mpsc::channel(1024);

        let (create_uni_tx, create_uni_rx) = mpsc::channel(8);
        let (create_bi_tx, create_bi_rx) = mpsc::channel(8);

        let (outbound_tx, outbound_rx) = mpsc::channel(8);
        let (outbound_priority_tx, outbound_priority_rx) = mpsc::unbounded_channel();

        let closed = watch::Sender::new(None);

        let mut backend = SessionState {
            transport,
            version,
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
        };
        tokio::spawn(async move {
            let err = backend.run().await.err().unwrap_or(Error::Closed);
            backend.closed.send(Some(err)).ok();
        });

        Session {
            is_server,
            outbound: outbound_tx,
            outbound_priority: outbound_priority_tx,
            accept_bi: Arc::new(tokio::sync::Mutex::new(accept_bi_rx)),
            accept_uni: Arc::new(tokio::sync::Mutex::new(accept_uni_rx)),
            create_uni: create_uni_tx,
            create_bi: create_bi_tx,
            create_uni_id: Default::default(),
            create_bi_id: Default::default(),
            closed,
            protocol,
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
        let id = self.create_uni_id.fetch_add(1, Ordering::Relaxed);
        let id = StreamId::new(id, StreamDir::Uni, self.is_server);

        let (tx, rx) = mpsc::unbounded_channel();
        let send_backend = SendState {
            inbound_stopped: tx,
        };
        let send_frontend = SendStream {
            id,
            outbound: self.outbound.clone(),
            outbound_priority: self.outbound_priority.clone(),
            inbound_stopped: rx,
            offset: 0,
            closed: None,
            fin: false,
        };

        self.create_uni
            .send((id, send_backend))
            .await
            .map_err(|_| Error::Closed)?;

        Ok(send_frontend)
    }

    async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
        let id = self.create_bi_id.fetch_add(1, Ordering::Relaxed);
        let id = StreamId::new(id, StreamDir::Bi, self.is_server);

        let (tx, rx) = mpsc::unbounded_channel();
        let (tx2, rx2) = mpsc::unbounded_channel();

        let send_backend = SendState {
            inbound_stopped: tx,
        };
        let send_frontend = SendStream {
            id,
            outbound: self.outbound.clone(),
            outbound_priority: self.outbound_priority.clone(),
            inbound_stopped: rx,
            offset: 0,
            closed: None,
            fin: false,
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let recv_backend = RecvState {
            inbound_data: tx,
            inbound_reset: tx2,
        };
        let recv_frontend = RecvStream {
            id,
            inbound_data: rx,
            inbound_reset: rx2,
            outbound_priority: self.outbound_priority.clone(),
            buffer: Bytes::new(),
            closed: None,
            fin: false,
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
        self.protocol.as_deref()
    }
}

struct SendState {
    inbound_stopped: mpsc::UnboundedSender<StopSending>,
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
}

impl SendStream {
    fn recv_stop(&mut self, code: VarInt) -> Error {
        if let Some(error) = &self.closed {
            return error.clone();
        }

        let frame = ResetStream { id: self.id, code };

        let error = Error::StreamStop(code);

        self.outbound_priority.send(frame.into()).ok();
        self.closed = Some(error.clone());

        error
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
            let chunk_size = buf.chunk().len().min(MAX_FRAME_PAYLOAD);
            let frame = Stream {
                id: self.id,
                data: buf.copy_to_bytes(chunk_size),
                fin: false,
            };

            tokio::select! {
                result = self.outbound.send(frame.into()) => {
                    if result.is_err() {
                        return Err(Error::Closed);
                    }
                    self.offset += chunk_size as u64;
                    total += chunk_size;
                }
                Some(stop) = self.inbound_stopped.recv() => {
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
        let frame = ResetStream { id: self.id, code };

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
}

/// The receive half of a multiplexed stream.
pub struct RecvStream {
    id: StreamId,

    outbound_priority: mpsc::UnboundedSender<Frame>, // STOP_SENDING
    inbound_data: mpsc::UnboundedReceiver<Stream>,
    inbound_reset: mpsc::UnboundedReceiver<ResetStream>,

    buffer: Bytes,

    closed: Option<Error>,
    fin: bool,
}

impl RecvStream {
    fn recv_reset(&mut self, code: VarInt) -> Error {
        if let Some(error) = &self.closed {
            return error.clone();
        }

        self.closed = Some(Error::StreamReset(code));
        Error::StreamReset(code)
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        if !self.fin && self.closed.is_none() {
            generic::RecvStream::stop(self, 0);
        }
    }
}

impl generic::RecvStream for RecvStream {
    type Error = Error;

    async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Self::Error> {
        loop {
            if !self.buffer.is_empty() {
                let to_read = max.min(self.buffer.len());
                return Ok(Some(self.buffer.split_to(to_read)));
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
