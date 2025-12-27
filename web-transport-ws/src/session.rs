use std::{
    collections::{hash_map, HashMap},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use crate::{tungstenite, ConnectionClose, ResetStream, StopSending, Stream, StreamDir, ALPN};
use crate::{Error, Frame, StreamId};
use bytes::{Buf, BufMut, Bytes};
use futures::{SinkExt, StreamExt};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, watch},
};
use tungstenite::{client::IntoClientRequest, handshake::server, http, Message};
use web_transport_proto::VarInt;
use web_transport_trait as generic;

/// Emulates a WebTransport session over a WebSocket connection.
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
}

struct SessionState<T> {
    ws: T,
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

impl<T> SessionState<T>
where
    T: futures::Stream<Item = Result<Message, tungstenite::Error>>
        + futures::Sink<Message, Error = tungstenite::Error>
        + Unpin,
{
    async fn run(&mut self) -> Result<(), Error> {
        let mut closed = self.closed.subscribe();

        loop {
            tokio::select! {
                biased;
                message = self.ws.next() => {
                    match message.ok_or(Error::Closed)?? {
                        Message::Binary(data) => {
                            let frame = Frame::decode(data.into())?;
                            self.recv_frame(frame).await?;
                        },
                        Message::Close(_) => {
                            self.closed
                                .send(Some(Error::Closed))
                                .ok();
                            return Ok(());
                        },
                        Message::Text(_) => {
                            return Err(Error::NoText);
                        },
                        Message::Ping(data) => {
                            self.ws.send(Message::Pong(data)).await?;
                        },
                        Message::Pong(_) => {
                            return Err(Error::NoPong);
                        },
                        Message::Frame(_) => {
                            return Err(Error::NoGenericFrames);
                        }
                    };
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

        let data = frame.encode();
        self.ws
            .send(Message::Binary(data.to_vec()))
            .await
            .map_err(|_| Error::Closed)?;

        Ok(())
    }

    async fn recv_frame(&mut self, frame: Frame) -> Result<(), Error> {
        match frame {
            Frame::Stream(stream) => {
                if !stream.id.can_recv(self.is_server) {
                    return Err(Error::InvalidStreamId);
                }

                match self.recv_streams.entry(stream.id) {
                    hash_map::Entry::Vacant(e) => {
                        if self.is_server == stream.id.server_initiated() {
                            // Already closed, ignore it. TODO slightly wrong
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
    pub fn new<T>(ws: T, is_server: bool) -> Self
    where
        T: futures::Stream<Item = Result<Message, tungstenite::Error>>
            + futures::Sink<Message, Error = tungstenite::Error>
            + Unpin
            + Send
            + 'static,
    {
        let (accept_bi_tx, accept_bi_rx) = mpsc::channel(1024);
        let (accept_uni_tx, accept_uni_rx) = mpsc::channel(1024);

        let (create_uni_tx, create_uni_rx) = mpsc::channel(8);
        let (create_bi_tx, create_bi_rx) = mpsc::channel(8);

        let (outbound_tx, outbound_rx) = mpsc::channel(8);
        let (outbound_priority_tx, outbound_priority_rx) = mpsc::unbounded_channel();

        let closed = watch::Sender::new(None);

        let mut backend = SessionState {
            ws,
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
        }
    }

    pub async fn accept<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
        socket: T,
    ) -> Result<Session, Error> {
        // Create callback to handle WebTransport protocol negotiation
        let callback = |req: &server::Request,
                        mut response: server::Response|
         -> Result<server::Response, server::ErrorResponse> {
            // Check for WebTransport subprotocol in Sec-WebSocket-Protocol header
            let protocols = req
                .headers()
                .get(http::header::SEC_WEBSOCKET_PROTOCOL)
                .and_then(|h| h.to_str().ok())
                .unwrap_or_default();

            if !protocols.split(',').any(|p| p.trim() == ALPN) {
                return Err(http::Response::builder()
                    .status(http::StatusCode::BAD_REQUEST)
                    .body(Some("'web-transport' protocol required".to_string()))
                    .unwrap());
            }

            // Add the selected protocol to the response
            response.headers_mut().insert(
                http::header::SEC_WEBSOCKET_PROTOCOL,
                http::HeaderValue::from_str(ALPN).unwrap(),
            );

            Ok(response)
        };

        let ws = tokio_tungstenite::accept_hdr_async_with_config(socket, callback, None).await?;
        Ok(Session::new(ws, true))
    }

    pub async fn connect(url: &str) -> Result<Session, Error> {
        let mut request = url.into_client_request()?;
        request.headers_mut().insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            http::HeaderValue::from_str(ALPN).unwrap(),
        );

        let (ws_stream, _) = tokio_tungstenite::connect_async(request).await?;
        Ok(Session::new(ws_stream, false))
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
        // Notify peer first
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
        todo!()
    }

    fn max_datagram_size(&self) -> usize {
        todo!()
    }

    async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
        todo!()
    }
}

struct SendState {
    inbound_stopped: mpsc::UnboundedSender<StopSending>,
}

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

        let size = buf.chunk().len();
        let frame = Stream {
            id: self.id,
            data: buf.copy_to_bytes(size),
            fin: false,
        };

        tokio::select! {
            result = self.outbound.send(frame.into()) => {
                if result.is_err() {
                    return Err(Error::Closed);
                }
                self.offset += size as u64;
                Ok(size)
            }
            Some(stop) = self.inbound_stopped.recv() => {
                Err(self.recv_stop(stop.code))
            }
        }
    }

    fn set_priority(&mut self, _priority: u8) {
        // Priority not implemented in this version
    }

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
            // This is a sync function so we need to spawn a task if we're blocked on sending the frame.
            // Thanks, I hate it.
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

        // NOTE: will be racey if this is not &mut

        match self.inbound_stopped.recv().await {
            Some(stop) => Err(self.recv_stop(stop.code)),
            None => Err(Error::Closed),
        }
    }
}

struct RecvState {
    inbound_data: mpsc::UnboundedSender<Stream>,
    inbound_reset: mpsc::UnboundedSender<ResetStream>,
}

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
            Some(data) if data.len() > 0 => {
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

            if !self.buffer.is_empty() {
                // We have buffered data, so there's no point waiting for more.
                return Err(match self.inbound_reset.recv().await {
                    Some(reset) => self.recv_reset(reset.code),
                    None => Error::Closed,
                });
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
