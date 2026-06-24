use std::{
    collections::{hash_map, HashMap},
    sync::{Arc, OnceLock},
};

use crate::config::Config;
use crate::credit::Credit;
use crate::sched::PriorityQueue;
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

    outbound: PriorityQueue,
    outbound_priority: mpsc::UnboundedSender<Frame>,

    accept_bi: Arc<tokio::sync::Mutex<mpsc::Receiver<(SendStream, RecvStream)>>>,
    accept_uni: Arc<tokio::sync::Mutex<mpsc::Receiver<RecvStream>>>,

    create_uni: mpsc::Sender<(StreamId, SendState)>,
    create_bi: mpsc::Sender<(StreamId, SendState, RecvState)>,

    closed: watch::Sender<Option<Error>>,

    // Negotiated application protocol (via the application_protocols transport
    // parameter). `None` inside the OnceLock means "no protocol"; the OnceLock
    // being unset means negotiation is still pending. `negotiated_ready` flips
    // to `true` once it's set, so `negotiated()` can await it.
    negotiated: Arc<OnceLock<Option<String>>>,
    negotiated_ready: watch::Receiver<bool>,

    // Path the peer advertised via the `path` transport parameter. The watch
    // holds `None` while unresolved, then `Some(path)` / `Some(None)` once the
    // peer's params arrive (or the connection closes). `path()` awaits that
    // transition; the value and the readiness signal live in the one channel.
    peer_path: watch::Receiver<Option<Option<String>>>,

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

    outbound: PriorityQueue,
    outbound_priority: (mpsc::UnboundedSender<Frame>, mpsc::UnboundedReceiver<Frame>),

    accept_bi: mpsc::Sender<(SendStream, RecvStream)>,
    accept_uni: mpsc::Sender<RecvStream>,

    create_uni: mpsc::Receiver<(StreamId, SendState)>,
    create_bi: mpsc::Receiver<(StreamId, SendState, RecvState)>,

    send_streams: HashMap<StreamId, SendState>,
    recv_streams: HashMap<StreamId, RecvState>,

    closed: watch::Sender<Option<Error>>,

    // Negotiated application protocol — see the matching fields on `Session`.
    negotiated: Arc<OnceLock<Option<String>>>,
    negotiated_ready: watch::Sender<bool>,

    // Peer's advertised path — see the matching field on `Session`.
    peer_path: watch::Sender<Option<Option<String>>>,

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

    // QMux01 idle-timeout state (engaged once we've received the peer's params)
    last_recv_at: tokio::time::Instant,
    last_send_at: tokio::time::Instant,
    next_ping_seq: u64,
}

impl<T: Transport> SessionState<T> {
    async fn run(&mut self) -> Result<(), Error> {
        // QMux requires TRANSPORT_PARAMETERS as the first frame on the connection.
        if self.config.version.is_qmux() {
            self.send_transport_parameters().await?;
        }

        let mut closed = self.closed.subscribe();

        loop {
            // Compute the effective idle timeout — the smaller of the two non-zero values.
            // While we're still waiting on the peer's transport parameters, treat the
            // timeout as disabled so we don't tear the connection down before negotiation.
            let idle_timeout_ms = self.effective_idle_timeout_ms();
            let idle_deadline =
                idle_timeout_ms.map(|ms| self.last_recv_at + std::time::Duration::from_millis(ms));
            // Keep-alive: send a QX_PING when we've been silent for a third of the timeout.
            // (Any frame counts as activity; this fires only when both sides are idle.)
            // Clamp to 1ms so a tiny configured timeout doesn't yield a zero-duration
            // deadline that fires every loop iteration.
            let ping_deadline = idle_timeout_ms
                .map(|ms| self.last_send_at + std::time::Duration::from_millis((ms / 3).max(1)));

            tokio::select! {
                biased;
                result = self.transport.recv() => {
                    let data = result?;
                    self.last_recv_at = tokio::time::Instant::now();
                    if self.config.version == Version::QMux01 {
                        // QMux01: data is a record containing one or more frames
                        for frame in Frame::decode_record(data)? {
                            self.recv_frame(frame).await?;
                        }
                    } else if let Some(frame) = Frame::decode(data, self.config.version)? {
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
                frame = self.outbound.pop() => {
                    match frame {
                        Some(frame) => self.send_frame(frame).await?,
                        None => return Err(Error::Closed),
                    };
                }
                _ = async { tokio::time::sleep_until(idle_deadline.unwrap()).await }, if idle_deadline.is_some() => {
                    tracing::debug!("idle timeout fired");
                    return Err(Error::IdleTimeout);
                }
                _ = async { tokio::time::sleep_until(ping_deadline.unwrap()).await }, if ping_deadline.is_some() => {
                    // Periodic keep-alive: send a QX_PING so the peer's idle timer resets.
                    let seq = self.next_ping_seq;
                    self.next_ping_seq = self.next_ping_seq.wrapping_add(1);
                    self.send_frame(Frame::Ping(crate::Ping { sequence: seq, response: false })).await?;
                }
                _ = async { closed.wait_for(|err| err.is_some()).await.ok(); } => {
                    return Err(closed.borrow().clone().unwrap_or(Error::Closed))
                }
            }
        }
    }

    /// Effective idle timeout in milliseconds, or `None` if disabled.
    ///
    /// Only kicks in for QMux01 after both sides have exchanged transport parameters.
    /// Per RFC 9000 §10.1, the effective timeout is `min(our, peer)` of the non-zero values
    /// (or the single non-zero one). If both are zero, idle timeouts are disabled.
    fn effective_idle_timeout_ms(&self) -> Option<u64> {
        if self.config.version != Version::QMux01 || !self.params_received {
            return None;
        }
        match (
            self.our_params.max_idle_timeout,
            self.peer_params.max_idle_timeout,
        ) {
            (0, 0) => None,
            (a, 0) | (0, a) => Some(a),
            (a, b) => Some(a.min(b)),
        }
    }

    /// Send a QX_TRANSPORT_PARAMETERS frame with our defaults.
    async fn send_transport_parameters(&mut self) -> Result<(), Error> {
        let frame = Frame::TransportParameters(self.our_params.clone());
        self.send_encoded(frame.encode(self.config.version)?).await
    }

    /// Send pre-encoded frame bytes, validating against the peer's
    /// `max_record_size` for QMux01. The transport handles any
    /// transport-level framing (size varint on TCP/TLS; implicit on WS).
    async fn send_encoded(&mut self, bytes: Bytes) -> Result<(), Error> {
        if self.config.version == Version::QMux01 {
            // Until the peer's TRANSPORT_PARAMETERS arrive, fall back to the draft-01
            // default so we don't accidentally send a record the peer must reject.
            let limit = if self.params_received {
                self.peer_params.max_record_size
            } else {
                crate::proto::DEFAULT_MAX_RECORD_SIZE
            };
            if bytes.len() as u64 > limit {
                return Err(Error::FrameTooLarge);
            }
        }
        self.transport.send(bytes).await?;
        self.last_send_at = tokio::time::Instant::now();
        Ok(())
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

        self.send_encoded(frame.encode(self.config.version)?).await
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
                        if self.config.version.is_qmux() {
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
                        let recv_window = if self.config.version.is_qmux() {
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

                        let recv_streams_credit = if self.config.version.is_qmux() {
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
                                    stream_credit: if self.config.version.is_qmux() {
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
                                    outbound: self.outbound.clone(),
                                    outbound_priority: self.outbound_priority.0.clone(),
                                    inbound_stopped: rx,
                                    offset: 0,
                                    priority: 0,
                                    closed: None,
                                    fin: false,
                                    stream_credit: send_backend.stream_credit.clone(),
                                    conn_credit: if self.config.version.is_qmux() {
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
            // PADDING is a no-op
            Frame::Padding => {}
            // QX_PING: respond to requests, ignore responses
            Frame::Ping(ping) => {
                if !ping.response {
                    let response = Frame::Ping(crate::Ping {
                        sequence: ping.sequence,
                        response: true,
                    });
                    self.outbound_priority.0.send(response).ok();
                }
            }
        }

        Ok(())
    }

    /// Record the negotiated application protocol exactly once and wake any
    /// `Session::negotiated()` waiters. Later calls are no-ops.
    fn resolve_negotiated(&self, protocol: Option<String>) {
        if self.negotiated.set(protocol).is_ok() {
            self.negotiated_ready.send_replace(true);
        }
    }

    /// Record the peer's advertised path exactly once and wake any
    /// `Session::path()` waiters. The first resolution — the peer's params or
    /// the connection closing — wins; later calls are no-ops.
    fn resolve_peer_path(&self, path: Option<String>) {
        self.peer_path.send_if_modified(|slot| {
            if slot.is_none() {
                *slot = Some(path);
                true
            } else {
                false
            }
        });
    }

    fn recv_transport_parameters(&mut self, params: TransportParams) -> Result<(), Error> {
        if self.params_received {
            // Duplicate transport parameters
            return Err(Error::FlowControlError);
        }
        self.params_received = true;

        // Resolve / validate the application protocol now the peer's offer is known.
        match &self.config.protocol {
            // In-band negotiation: pick the agreed protocol (server preference
            // wins, matching RFC 7301). The OnceLock is still pending here.
            crate::Protocol::Negotiate(ours) => {
                let agreed = negotiate_protocol(self.is_server, ours, &params.protocols);
                self.resolve_negotiated(agreed);
            }
            // Not negotiating in-band: the peer MUST NOT send the parameter.
            // TLS/WebSocket already chose a protocol via ALPN, and a session
            // that didn't opt in has no way to interpret it — either way it's a
            // protocol error. (The OnceLock was resolved eagerly at construction.)
            crate::Protocol::None | crate::Protocol::Negotiated(_) => {
                if !params.protocols.is_empty() {
                    return Err(Error::UnexpectedProtocols);
                }
            }
        }

        // Surface the peer's path (if any). Unlike protocols this isn't gated on
        // opt-in: it's optional routing metadata, so an absent param is just None.
        self.resolve_peer_path(params.path.clone());

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

    /// Wait for the negotiated application protocol.
    ///
    /// For [`Protocol::Negotiated`](crate::Protocol::Negotiated) /
    /// [`Protocol::None`](crate::Protocol::None) (e.g. TLS/WS ALPN) this resolves
    /// immediately. For [`Protocol::Negotiate`](crate::Protocol::Negotiate) — the
    /// in-band path over TCP, Unix sockets, or any
    /// [`Stream`](crate::transport::Stream)-based session — it resolves once the
    /// peer's transport parameters arrive, or to `None` if the connection closes
    /// first.
    ///
    /// [`Session::protocol`](web_transport_trait::Session::protocol) returns the
    /// same value synchronously, but yields `None` until negotiation completes.
    pub async fn negotiated(&self) -> Option<String> {
        if self.negotiated.get().is_none() {
            // `wait_for` checks the current value first, so this can't miss the
            // transition even if it already happened.
            let mut ready = self.negotiated_ready.clone();
            ready.wait_for(|&done| done).await.ok();
        }
        self.negotiated.get().cloned().flatten()
    }

    /// The path the peer advertised via the `path` transport parameter.
    ///
    /// For a server this is the resource path the client requested; for a client
    /// it's whatever the server advertised (usually `None`).
    ///
    /// This is async because the value rides in the peer's transport parameters,
    /// which arrive after the connection is established. It resolves once those
    /// params arrive (carrying a path or nothing), or to `None` if the connection
    /// closes first. For the legacy `webtransport` wire format — which exchanges
    /// no parameters — it resolves to `None` immediately.
    pub async fn path(&self) -> Option<String> {
        let mut rx = self.peer_path.clone();
        // `wait_for` checks the current value first, so this can't miss a
        // resolution that already happened. `.ok()` covers the sender dropping
        // without resolving — `flatten()` then yields `None`.
        rx.wait_for(|slot| slot.is_some()).await.ok();
        let resolved = rx.borrow().clone();
        resolved.flatten()
    }

    fn new<T: Transport>(transport: T, is_server: bool, config: Config) -> Self {
        let version = config.version;
        let our_params = config.to_transport_params();

        let (accept_bi_tx, accept_bi_rx) = mpsc::channel(1024);
        let (accept_uni_tx, accept_uni_rx) = mpsc::channel(1024);

        let (create_uni_tx, create_uni_rx) = mpsc::channel(8);
        let (create_bi_tx, create_bi_rx) = mpsc::channel(8);

        let outbound = PriorityQueue::new(8);
        let (outbound_priority_tx, outbound_priority_rx) = mpsc::unbounded_channel();

        let closed = watch::Sender::new(None);

        // Protocol negotiation. Only `Negotiate` resolves in-band (once the
        // peer's params arrive); the out-of-band cases resolve immediately.
        let negotiated: Arc<OnceLock<Option<String>>> = Arc::new(OnceLock::new());
        let (negotiated_ready_tx, negotiated_ready_rx) = watch::channel(false);
        match &config.protocol {
            crate::Protocol::Negotiate(_) => {} // pending
            crate::Protocol::Negotiated(name) => {
                negotiated.set(Some(name.clone())).ok();
                negotiated_ready_tx.send_replace(true);
            }
            crate::Protocol::None => {
                negotiated.set(None).ok();
                negotiated_ready_tx.send_replace(true);
            }
        }

        // Peer path. Unresolved (`None`) until the peer's transport parameters
        // arrive. Only QMux versions exchange params; the legacy `webtransport`
        // format never does, so resolve to `Some(None)` eagerly to keep `path()`
        // from hanging.
        let initial_peer_path = if version.is_qmux() { None } else { Some(None) };
        let (peer_path_tx, peer_path_rx) = watch::channel(initial_peer_path);

        let open_bi_credit = Credit::new(if version.is_qmux() { 0 } else { u64::MAX });
        let open_uni_credit = Credit::new(if version.is_qmux() { 0 } else { u64::MAX });

        let conn_send_credit = Credit::new(if version.is_qmux() { 0 } else { u64::MAX });

        let conn_recv_credit = Credit::new(if version.is_qmux() {
            our_params.initial_max_data
        } else {
            u64::MAX
        });

        // Stream count credits for incoming streams
        let recv_bi_credit = Credit::new(if version.is_qmux() {
            config.max_streams_bidi
        } else {
            u64::MAX
        });
        let recv_uni_credit = Credit::new(if version.is_qmux() {
            config.max_streams_uni
        } else {
            u64::MAX
        });

        let mut backend = SessionState {
            transport,
            config: config.clone(),
            outbound: outbound.clone(),
            outbound_priority: (outbound_priority_tx.clone(), outbound_priority_rx),
            accept_bi: accept_bi_tx,
            accept_uni: accept_uni_tx,
            create_uni: create_uni_rx,
            create_bi: create_bi_rx,
            is_server,
            send_streams: HashMap::new(),
            recv_streams: HashMap::new(),
            closed: closed.clone(),
            negotiated: negotiated.clone(),
            negotiated_ready: negotiated_ready_tx,
            peer_path: peer_path_tx,
            conn_send_credit: conn_send_credit.clone(),
            conn_recv_credit: conn_recv_credit.clone(),
            our_params: our_params.clone(),
            peer_params: TransportParams::default(),
            params_received: false,
            open_bi_credit: open_bi_credit.clone(),
            open_uni_credit: open_uni_credit.clone(),
            recv_bi_credit: recv_bi_credit.clone(),
            recv_uni_credit: recv_uni_credit.clone(),
            last_recv_at: tokio::time::Instant::now(),
            last_send_at: tokio::time::Instant::now(),
            next_ping_seq: 0,
        };
        tokio::spawn(async move {
            let err = backend.run().await.err().unwrap_or(Error::Closed);
            // Unblock any `negotiated()` / `peer_path()` waiter if the connection
            // died before the peer's params arrived. No-op for already-resolved cases.
            backend.resolve_negotiated(None);
            backend.resolve_peer_path(None);
            // Close all credits so blocked claim()/claim_index() calls unblock
            backend.open_bi_credit.close();
            backend.open_uni_credit.close();
            backend.conn_send_credit.close();
            backend.conn_recv_credit.close();
            backend.outbound.close();
            for send in backend.send_streams.values() {
                if let Some(credit) = &send.stream_credit {
                    credit.close();
                }
            }
            // `send_replace`, not `send`: the latter drops the value when there
            // are no receivers, which loses the close reason for any `closed()`
            // call made after the session has already finished closing (e.g. after
            // awaiting `path()` on a peer that closed without sending params).
            // Storing it unconditionally keeps late waiters correct.
            backend.closed.send_replace(Some(err));
        });

        Session {
            is_server,
            config,
            outbound,
            outbound_priority: outbound_priority_tx,
            accept_bi: Arc::new(tokio::sync::Mutex::new(accept_bi_rx)),
            accept_uni: Arc::new(tokio::sync::Mutex::new(accept_uni_rx)),
            create_uni: create_uni_tx,
            create_bi: create_bi_tx,
            closed,
            negotiated,
            negotiated_ready: negotiated_ready_rx,
            peer_path: peer_path_rx,
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

        let stream_credit = if self.config.version.is_qmux() {
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
            priority: 0,
            closed: None,
            fin: false,
            stream_credit,
            conn_credit: if self.config.version.is_qmux() {
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

        let stream_credit = if self.config.version.is_qmux() {
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
            priority: 0,
            closed: None,
            fin: false,
            stream_credit,
            conn_credit: if self.config.version.is_qmux() {
                Some(self.conn_send_credit.clone())
            } else {
                None
            },
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let recv_window = if self.config.version.is_qmux() {
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
        // The OnceLock holds the resolved protocol (out-of-band cases are set at
        // construction). `None` here means in-band negotiation is still pending.
        self.negotiated.get().and_then(|p| p.as_deref())
    }
}

/// Select the agreed application protocol from two advertised lists.
///
/// The server's preference order wins (first server entry the client also
/// offered), matching RFC 7301 ALPN selection. Both peers compute the same
/// answer because each knows whether it is the server. Returns `None` when the
/// lists don't overlap (or either side advertised nothing).
fn negotiate_protocol(is_server: bool, ours: &[String], peers: &[String]) -> Option<String> {
    let (server, client) = if is_server {
        (ours, peers)
    } else {
        (peers, ours)
    };
    server.iter().find(|p| client.contains(p)).cloned()
}

struct SendState {
    inbound_stopped: mpsc::UnboundedSender<StopSending>,
    stream_credit: Option<Credit>,
}

/// The send half of a multiplexed stream.
pub struct SendStream {
    id: StreamId,

    outbound: PriorityQueue,                         // STREAM
    outbound_priority: mpsc::UnboundedSender<Frame>, // RESET_STREAM
    inbound_stopped: mpsc::UnboundedReceiver<StopSending>,

    offset: u64,
    /// Scheduling priority (higher = sent first). Threaded into the queue on
    /// every `push` and relayed to the queue on `set_priority`.
    priority: u8,
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

        let error = Error::StreamStop(code);

        // If we've already sent a FIN, the stream is finished; don't also emit a
        // RESET_STREAM for it (that would put two terminal frames on one stream).
        if !self.fin {
            let frame = ResetStream {
                id: self.id,
                code,
                final_size: self.offset,
            };
            self.outbound_priority.send(frame.into()).ok();
        }
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
                result = self.outbound.push(self.priority, self.id, frame.into()) => {
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

    /// Set the stream's send priority; higher values are sent first.
    ///
    /// Re-prioritization is retroactive: already-queued frames for this stream
    /// move to the new band on the next scheduling decision (the bytes stay put,
    /// preserving per-stream order).
    fn set_priority(&mut self, order: u8) {
        self.priority = order;
        self.outbound.set_priority(self.id, order);
    }

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

        // Enqueue the FIN synchronously into the stream's band (after its data),
        // bypassing the capacity bound. This avoids detaching it to a task, which
        // could race a concurrent reset/stop (emitting RESET_STREAM and then a
        // FIN) and would also hide a closed queue behind a successful return.
        self.outbound
            .push_now(self.priority, self.id, frame.into())?;
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
        if !self.version.is_qmux() {
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

#[cfg(test)]
mod negotiate_tests {
    use super::negotiate_protocol;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn server_preference_wins() {
        let server = v(&["b", "a"]);
        let client = v(&["a", "b"]);
        // Server is the authority, so its order ("b" first) decides.
        assert_eq!(
            negotiate_protocol(true, &server, &client).as_deref(),
            Some("b")
        );
        // Same inputs from the client's vantage point must agree.
        assert_eq!(
            negotiate_protocol(false, &client, &server).as_deref(),
            Some("b")
        );
    }

    #[test]
    fn no_overlap_is_none() {
        assert_eq!(negotiate_protocol(true, &v(&["a"]), &v(&["b"])), None);
        assert_eq!(negotiate_protocol(true, &v(&["a"]), &[]), None);
    }
}
