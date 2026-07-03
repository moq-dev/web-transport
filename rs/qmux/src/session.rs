use std::{
    collections::{hash_map, HashMap, HashSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock,
    },
};

use crate::config::Config;
use crate::credit::Credit;
use crate::sched::PriorityQueue;
use crate::transport::Transport;
use crate::{
    proto::varint_size, ConnectionClose, Error, Frame, ResetStream, StopSending, Stream, StreamDir,
    StreamId, TransportParams, Version, MAX_FRAME_PAYLOAD,
};
use bytes::{Buf, BufMut, Bytes};
use tokio::sync::{mpsc, watch};
use web_transport_proto::VarInt;
use web_transport_trait as generic;

/// How many inbound datagrams to buffer before dropping. Datagrams are
/// unreliable, so a slow `recv_datagram` consumer sheds load here rather than
/// applying backpressure to the whole session.
const DATAGRAM_RECV_BUFFER: usize = 1024;

/// How many outbound datagrams to buffer before dropping. When the transport is
/// backpressured, `send_datagram` sheds datagrams here rather than growing an
/// unbounded queue — droppable is the whole point of an unreliable datagram.
const DATAGRAM_SEND_BUFFER: usize = 1024;

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
    // parameter). Resolved exactly once, before the session is handed to the
    // caller (see `established()`), so `protocol()` is a plain synchronous
    // getter. `None` inside the OnceLock means "no value"; unset means the peer's
    // params haven't arrived yet (only observable on a session you constructed
    // without awaiting `established()`). The OnceLock gives the resolved value a
    // stable address so the getter can hand out a `&str` borrow.
    negotiated: Arc<OnceLock<Option<String>>>,

    // Flips to `true` once the peer's transport parameters have been received and
    // applied (or eagerly for the param-less `webtransport` format). `established()`
    // awaits this; if the sender drops first, the connection closed mid-handshake.
    established: watch::Receiver<bool>,

    // Flow control: stream count credits (claim_index returns stream sequence number)
    open_bi_credit: Credit,
    open_uni_credit: Credit,

    // Shared connection-level send credit (shared with SendStreams)
    conn_send_credit: Credit,

    // Shared connection-level recv credit (shared with RecvStreams)
    conn_recv_credit: Credit,

    // Inbound datagrams (RFC 9221). The backend fans DATAGRAM frames into this
    // channel; `recv_datagram` drains it. Bounded and lossy — a slow reader
    // drops datagrams rather than stalling the session.
    recv_datagram: Arc<tokio::sync::Mutex<mpsc::Receiver<Bytes>>>,

    // Outbound datagrams. `send_datagram` pushes payloads here; the backend loop
    // frames and writes them. Bounded and lossy so a backpressured transport
    // drops datagrams instead of queueing them unboundedly. Kept off the
    // (lossless) control lane, which must never drop RESET/STOP/CLOSE frames.
    outbound_datagram: mpsc::Sender<Bytes>,

    // The largest datagram payload we may send, i.e. `max_datagram_size()`.
    // Resolved from the peer's transport parameters before the session is handed
    // to the caller (0 = the peer doesn't accept datagrams).
    datagram_max_size: Arc<AtomicUsize>,
}

/// Tracks which peer-initiated recv-stream indices (in one direction) are open,
/// closed, or merely implicitly opened, so a frame on an id can be classified.
///
/// A peer opening stream index N implicitly opens all lower indices too (QUIC
/// RFC 9000 §3.2), and frames for different streams can arrive in any order. So a
/// vacant id below the high-water mark is ambiguous: it may have been created and
/// then retired (a duplicate/late frame to ignore) or implicitly opened and not
/// yet delivered (a genuinely new stream). We disambiguate by recording the
/// highest index we've instantiated a frontend for plus the still-unopened
/// "holes" beneath it.
#[derive(Default)]
struct RecvOpen {
    /// Highest index we've instantiated a frontend for (`None` = none yet).
    created_max: Option<u64>,
    /// Indices `<= created_max` that were implicitly opened (a higher index
    /// arrived first) but haven't had their own first frame, so no frontend
    /// exists yet. Bounded by MAX_STREAMS: a hole never replenishes stream-count
    /// credit until it's created, so the peer can't outrun its stream limit.
    holes: HashSet<u64>,
}

impl RecvOpen {
    /// Whether a frame for `index` targets an already-closed stream: one we
    /// created before (`index <= created_max` and not a still-open hole) but that
    /// is no longer live. Callers check the active map for liveness separately.
    fn is_closed(&self, index: u64) -> bool {
        matches!(self.created_max, Some(max) if index <= max) && !self.holes.contains(&index)
    }

    /// Record that `index` has been opened — a STREAM frontend instantiated for
    /// it, or a RESET_STREAM consuming it — advancing the high-water mark and
    /// filling in the holes it implicitly opened.
    fn record(&mut self, index: u64) {
        match self.created_max {
            // Filling a previously-implicit hole below the high-water mark.
            Some(max) if index <= max => {
                self.holes.remove(&index);
            }
            // New high-water mark: everything between the old mark and this index
            // is now implicitly opened but not yet delivered.
            prev => {
                let start = prev.map_or(0, |max| max + 1);
                self.holes.extend(start..index);
                self.created_max = Some(index);
            }
        }
    }
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

    // Negotiated protocol and handshake-complete signal — see the matching
    // fields on `Session`.
    negotiated: Arc<OnceLock<Option<String>>>,
    established: watch::Sender<bool>,

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

    // Open/closed bookkeeping for peer-initiated recv streams, per direction, so a
    // post-terminal frame on a retired id is ignored rather than resurrecting a
    // brand-new accepted stream. See `RecvOpen`. QMux only: MAX_STREAMS flow
    // control bounds the hole set to at most the peer's stream limit.
    recv_open_bi: RecvOpen,
    recv_open_uni: RecvOpen,

    // QMux01 idle-timeout state (engaged once we've received the peer's params)
    last_recv_at: tokio::time::Instant,
    last_send_at: tokio::time::Instant,
    next_ping_seq: u64,

    // Inbound datagram sink (see the matching field on `Session`) plus the
    // shared send-limit cell resolved from the peer's params.
    recv_datagram: mpsc::Sender<Bytes>,
    datagram_max_size: Arc<AtomicUsize>,

    // Receiver for outbound datagrams enqueued by `send_datagram`.
    outbound_datagram: mpsc::Receiver<Bytes>,
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
                Some(payload) = self.outbound_datagram.recv() => {
                    // Ahead of bulk stream data for low latency, behind control.
                    self.send_frame(Frame::Datagram(payload.into())).await?;
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

    /// Per-direction open/closed bookkeeping for peer-initiated recv streams.
    fn recv_open(&self, dir: StreamDir) -> &RecvOpen {
        match dir {
            StreamDir::Bi => &self.recv_open_bi,
            StreamDir::Uni => &self.recv_open_uni,
        }
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

                // Ignore post-terminal frames on an already-closed peer-initiated
                // stream. Once we deliver a FIN/RESET the entry is dropped, so
                // without this a duplicate/late STREAM frame would fall through and
                // be treated as a brand-new accepted stream (stream resurrection).
                // `is_closed` distinguishes a retired id from one that was only
                // implicitly opened (a higher index arrived first) and hasn't had
                // its own first frame yet — the latter is still new. Locally-
                // initiated ids are left to the `is_server` guard below; only QMux
                // tracks this, where MAX_STREAMS bounds the hole set.
                if self.config.version.is_qmux()
                    && stream.id.server_initiated() != self.is_server
                    && !self.recv_streams.contains_key(&stream.id)
                    && self.recv_open(stream.id.dir()).is_closed(stream.id.index())
                {
                    return Ok(());
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

                            // Record that we're instantiating a frontend for this
                            // id, so a later frame on it (after it's retired) is
                            // recognized as closed rather than resurrected. Runs
                            // after the credit gate, which bounds the hole set to at
                            // most MAX_STREAMS. Reached only for peer-initiated ids
                            // (the `is_server` guard above returned otherwise).
                            // Access the field directly (not via `recv_open_mut`)
                            // so the borrow stays disjoint from the `recv_streams`
                            // entry held open by this match.
                            match stream.id.dir() {
                                StreamDir::Bi => &mut self.recv_open_bi,
                                StreamDir::Uni => &mut self.recv_open_uni,
                            }
                            .record(stream.id.index());
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

                if let Some(recv) = self.recv_streams.remove(&reset.id) {
                    // Live stream: deliver the reset and drop it. It was recorded
                    // in `recv_open` at creation, so it now reads as closed.
                    recv.inbound_reset.send(reset).ok();
                } else if self.config.version.is_qmux()
                    && reset.id.server_initiated() != self.is_server
                {
                    // RESET_STREAM can be the *first* frame for a peer-initiated
                    // stream (it implicitly opens the id). Record it as closed so a
                    // later STREAM on the same id is recognized as retired rather
                    // than resurrected into a new accepted stream. Gate on the
                    // stream limit first, mirroring the STREAM path, so the hole set
                    // stays bounded by MAX_STREAMS. Locally-initiated ids are left
                    // to the `is_server` guard on the STREAM path.
                    let credit = match reset.id.dir() {
                        StreamDir::Bi => &self.recv_bi_credit,
                        StreamDir::Uni => &self.recv_uni_credit,
                    };
                    if !credit.receive_up_to(reset.id.index() + 1) {
                        return Err(Error::StreamLimitExceeded);
                    }
                    match reset.id.dir() {
                        StreamDir::Bi => &mut self.recv_open_bi,
                        StreamDir::Uni => &mut self.recv_open_uni,
                    }
                    .record(reset.id.index());
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
            // DATAGRAM: fan out to the receive channel. `max_datagram_frame_size`
            // caps the whole *frame* (type byte + length varint + payload), not
            // just the payload, so compare against the frame's exact encoded size
            // — which depends on the wire form (`Datagram::frame_size` accounts
            // for the 0x30 no-length form having no length varint). A peer that
            // sends a datagram we never advertised support for, or one that
            // overflows the negotiated limit, is a protocol violation — surface it
            // rather than silently dropping. Delivery past that is best-effort, so
            // drop the datagram if the channel is full rather than blocking the
            // session loop.
            Frame::Datagram(datagram) => {
                if self.our_params.max_datagram_frame_size == 0 {
                    return Err(Error::DatagramsUnsupported);
                }
                if datagram.frame_size() > self.our_params.max_datagram_frame_size {
                    return Err(Error::FrameTooLarge);
                }
                let _ = self.recv_datagram.try_send(datagram.data);
            }
        }

        Ok(())
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
                self.negotiated.set(agreed).ok();
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

        // Resolve the datagram send limit. Datagrams are a QMux01-only feature
        // (they rely on the record layer for framing), so they stay disabled on
        // any other wire format. Otherwise whether we may *send* depends solely on
        // the peer's willingness to receive (RFC 9221): 0 means the peer omitted
        // (or zeroed) max_datagram_frame_size.
        let datagram_max =
            if self.config.version != Version::QMux01 || params.max_datagram_frame_size == 0 {
                0
            } else {
                // A datagram must fit in one record, so the frame is capped by the
                // smaller of the peer's datagram-frame limit and its record size.
                let cap = params.max_datagram_frame_size.min(params.max_record_size);
                // We encode the length-prefixed form (0x31): one type byte plus a
                // length varint. `varint_size(cap)` bounds the varint for any payload
                // that fits in `cap`, so subtracting it keeps the encoded frame within
                // the peer's limit regardless of the exact payload length.
                let overhead = 1 + varint_size(cap);
                usize::try_from(cap.saturating_sub(overhead)).unwrap_or(usize::MAX)
            };
        // Store before signalling establishment so `connect`/`accept` callers
        // observe the resolved value via `max_datagram_size()`.
        self.datagram_max_size
            .store(datagram_max, Ordering::Release);

        self.peer_params = params;

        // Handshake complete: `negotiated` is now set, so unblock `established()`
        // and let the synchronous getter return its final value.
        self.established.send_replace(true);

        Ok(())
    }
}

impl Session {
    /// Open a client-side session over the given transport, waiting until it is
    /// established before returning.
    ///
    /// "Established" means the peer's transport parameters have been received and
    /// applied, so [`protocol`](web_transport_trait::Session::protocol) returns
    /// its final value. The legacy `webtransport` wire format exchanges no
    /// parameters, so it is established immediately.
    ///
    /// Bounded by [`Config::handshake_timeout`](crate::Config::handshake_timeout):
    /// if the peer completes the transport handshake but never sends its
    /// parameters, this returns [`Error::HandshakeTimeout`] rather than hanging;
    /// a mid-handshake disconnect returns the close reason.
    pub async fn connect<T: Transport>(transport: T, config: Config) -> Result<Session, Error> {
        let session = Self::new(transport, false, config);
        session.established().await?;
        Ok(session)
    }

    /// Open a server-side session over the given transport, waiting until it is
    /// established before returning. See [`Session::connect`] for the semantics.
    pub async fn accept<T: Transport>(transport: T, config: Config) -> Result<Session, Error> {
        let session = Self::new(transport, true, config);
        session.established().await?;
        Ok(session)
    }

    /// Wait until the peer's transport parameters have been received and applied.
    /// Folded into [`connect`](Session::connect) / [`accept`](Session::accept);
    /// see those for the timeout and error semantics.
    async fn established(&self) -> Result<(), Error> {
        let mut established = self.established.clone();
        if *established.borrow() {
            return Ok(());
        }

        let wait = established.wait_for(|&done| done);
        let timeout = self.config.handshake_timeout;
        // A zero timeout disables the bound (wait indefinitely).
        let outcome = if timeout.is_zero() {
            Some(wait.await)
        } else {
            tokio::time::timeout(timeout, wait).await.ok()
        };

        match outcome {
            // Established.
            Some(Ok(_)) => Ok(()),
            // The backend task ended before establishing — surface the close reason.
            Some(Err(_)) => Err(self.closed.borrow().clone().unwrap_or(Error::Closed)),
            // Timed out waiting for the peer's parameters: abort the half-open
            // handshake, notifying the peer, and fail rather than hang.
            None => {
                let _ = self.outbound_priority.send(
                    ConnectionClose {
                        code: VarInt::from(0u32),
                        reason: "handshake timeout".to_string(),
                    }
                    .into(),
                );
                self.closed.send_replace(Some(Error::HandshakeTimeout));
                Err(Error::HandshakeTimeout)
            }
        }
    }

    /// Construct a session over the transport and start its run loop, without
    /// waiting for the handshake. The public entry points are the async
    /// [`connect`](Session::connect) / [`accept`](Session::accept), which await
    /// establishment; this is for callers that resolve their protocol out of band
    /// (e.g. the WebSocket transport, which negotiates via the subprotocol).
    pub(crate) fn new<T: Transport>(transport: T, is_server: bool, config: Config) -> Self {
        let version = config.version;
        let our_params = config.to_transport_params();

        let (accept_bi_tx, accept_bi_rx) = mpsc::channel(1024);
        let (accept_uni_tx, accept_uni_rx) = mpsc::channel(1024);

        let (create_uni_tx, create_uni_rx) = mpsc::channel(8);
        let (create_bi_tx, create_bi_rx) = mpsc::channel(8);

        let outbound = PriorityQueue::new(8);
        let (outbound_priority_tx, outbound_priority_rx) = mpsc::unbounded_channel();

        // Bounded, lossy inbound-datagram channel: the backend drops on a full
        // buffer rather than stalling, matching QUIC's unreliable semantics.
        let (recv_datagram_tx, recv_datagram_rx) = mpsc::channel(DATAGRAM_RECV_BUFFER);
        let (outbound_datagram_tx, outbound_datagram_rx) = mpsc::channel(DATAGRAM_SEND_BUFFER);
        let datagram_max_size = Arc::new(AtomicUsize::new(0));

        let closed = watch::Sender::new(None);

        // Protocol negotiation. Only `Negotiate` resolves in-band (once the
        // peer's params arrive); the out-of-band cases resolve immediately.
        let negotiated: Arc<OnceLock<Option<String>>> = Arc::new(OnceLock::new());
        match &config.protocol {
            crate::Protocol::Negotiate(_) => {} // pending
            crate::Protocol::Negotiated(name) => {
                negotiated.set(Some(name.clone())).ok();
            }
            crate::Protocol::None => {
                negotiated.set(None).ok();
            }
        }

        // Handshake-complete signal. QMux versions flip it once the peer's params
        // arrive; the legacy `webtransport` format exchanges none, so it (and the
        // resolved getter) are established eagerly.
        let (established_tx, established_rx) = watch::channel(!version.is_qmux());

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
            established: established_tx,
            conn_send_credit: conn_send_credit.clone(),
            conn_recv_credit: conn_recv_credit.clone(),
            our_params: our_params.clone(),
            peer_params: TransportParams::default(),
            params_received: false,
            open_bi_credit: open_bi_credit.clone(),
            open_uni_credit: open_uni_credit.clone(),
            recv_bi_credit: recv_bi_credit.clone(),
            recv_uni_credit: recv_uni_credit.clone(),
            recv_open_bi: RecvOpen::default(),
            recv_open_uni: RecvOpen::default(),
            last_recv_at: tokio::time::Instant::now(),
            last_send_at: tokio::time::Instant::now(),
            next_ping_seq: 0,
            recv_datagram: recv_datagram_tx,
            datagram_max_size: datagram_max_size.clone(),
            outbound_datagram: outbound_datagram_rx,
        };
        tokio::spawn(async move {
            let err = backend.run().await.err().unwrap_or(Error::Closed);
            // Dropping `backend` drops the `established` sender; an `established()`
            // waiter that was still pending then observes the channel close and
            // reports this terminal error. The OnceLock stays unset, so the
            // synchronous getter reports `None` on a never-established session.
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
            // awaiting establishment on a peer that closed without sending params).
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
            established: established_rx,
            open_bi_credit,
            open_uni_credit,
            conn_send_credit,
            conn_recv_credit,
            recv_datagram: Arc::new(tokio::sync::Mutex::new(recv_datagram_rx)),
            datagram_max_size,
            outbound_datagram: outbound_datagram_tx,
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

    fn send_datagram(&self, payload: Bytes) -> Result<(), Self::Error> {
        let max = self.datagram_max_size.load(Ordering::Acquire);
        if max == 0 {
            // The peer never advertised max_datagram_frame_size (or zeroed it).
            return Err(Error::DatagramsUnsupported);
        }
        if payload.len() > max {
            return Err(Error::FrameTooLarge);
        }
        // Best-effort and synchronous, matching the trait's fire-and-forget
        // contract. A full buffer means the transport is backpressured: drop the
        // datagram (returning `Ok`) rather than block or grow without bound — an
        // unreliable datagram is meant to be droppable. A closed session (the
        // receiver is gone) surfaces as `Closed`.
        match self.outbound_datagram.try_send(payload) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(Error::Closed),
        }
    }

    fn max_datagram_size(&self) -> usize {
        self.datagram_max_size.load(Ordering::Acquire)
    }

    async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
        self.recv_datagram
            .lock()
            .await
            .recv()
            .await
            .ok_or(Error::Closed)
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

#[cfg(test)]
mod recv_open_tests {
    use std::time::Duration;

    use bytes::Bytes;
    use tokio::sync::mpsc;
    use web_transport_trait::{RecvStream as _, Session as _};

    use web_transport_proto::VarInt;

    use super::{Session, Transport};
    use crate::proto::{Frame, ResetStream, Stream};
    use crate::{Config, Error, StreamDir, StreamId, Version};

    /// A transport whose inbound frames are scripted through a channel; outbound
    /// writes are discarded. Once the script is drained, `recv` parks forever so
    /// the session's run loop keeps running (rather than seeing a closed
    /// transport and tearing down).
    struct ScriptedTransport {
        incoming: mpsc::UnboundedReceiver<Bytes>,
    }

    impl Transport for ScriptedTransport {
        async fn send(&mut self, _data: Bytes) -> Result<(), Error> {
            Ok(())
        }

        async fn recv(&mut self) -> Result<Bytes, Error> {
            match self.incoming.recv().await {
                Some(bytes) => Ok(bytes),
                None => std::future::pending().await,
            }
        }

        async fn close(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

    /// A client session fed by a scripted transport, plus the sender for inbound
    /// frames. QMux01, where the closed-stream tracking is active.
    fn scripted_session() -> (Session, mpsc::UnboundedSender<Bytes>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = Session::new(
            ScriptedTransport { incoming: rx },
            false,
            Config::new(Version::QMux01),
        );
        (session, tx)
    }

    /// Encode a STREAM frame on a server-initiated uni stream (peer-initiated and
    /// receivable, since we're the client).
    fn uni_stream(index: u64, data: &'static [u8], fin: bool) -> Bytes {
        Frame::Stream(Stream {
            id: StreamId::new(index, StreamDir::Uni, true),
            data: Bytes::from_static(data),
            fin,
        })
        .encode(Version::QMux01)
        .unwrap()
    }

    /// Encode a RESET_STREAM frame on a server-initiated uni stream.
    fn uni_reset(index: u64) -> Bytes {
        Frame::ResetStream(ResetStream {
            id: StreamId::new(index, StreamDir::Uni, true),
            code: VarInt::from_u32(0),
            final_size: 0,
        })
        .encode(Version::QMux01)
        .unwrap()
    }

    /// Regression test for the #274 stream-resurrection bug: after a
    /// peer-initiated recv stream is retired by a FIN, a duplicate/late STREAM
    /// frame on the same id must be ignored, not turned into a brand-new accepted
    /// stream.
    #[tokio::test]
    async fn retired_recv_stream_is_not_resurrected() {
        let (session, tx) = scripted_session();

        // Open with data, FIN (retires the stream), then a late frame on the same id.
        tx.send(uni_stream(0, b"hello", false)).unwrap();
        tx.send(uni_stream(0, b"", true)).unwrap();
        tx.send(uni_stream(0, b"late", false)).unwrap();

        // The peer's stream is accepted exactly once; drain it to EOF.
        let mut recv = tokio::time::timeout(Duration::from_secs(1), session.accept_uni())
            .await
            .expect("accept_uni timed out")
            .expect("accept_uni failed");
        assert_eq!(recv.read_all().await.unwrap().as_ref(), b"hello");

        // The late frame must be dropped: no second stream shows up on the queue.
        let second = tokio::time::timeout(Duration::from_millis(200), session.accept_uni()).await;
        assert!(
            second.is_err(),
            "a late frame on a retired stream resurrected a new accepted stream"
        );
    }

    /// A FIN for a higher stream index arriving before the first frame of a lower
    /// one must NOT retire the lower stream. Opening index 10 only *implicitly*
    /// opens 0..10 — it doesn't close them — so a later first frame on index 6 is a
    /// real, new stream, not a post-terminal one. (Guards against a naive
    /// highest-retired-index tombstone wrongly dropping it.)
    #[tokio::test]
    async fn implicitly_opened_lower_stream_is_still_accepted() {
        let (session, tx) = scripted_session();

        // Retire stream 10 first, then deliver the first frame of stream 6.
        tx.send(uni_stream(10, b"", true)).unwrap();
        tx.send(uni_stream(6, b"hello", true)).unwrap();

        // Stream 10 arrived first, so it's accepted first, and it's empty.
        let mut first = tokio::time::timeout(Duration::from_secs(1), session.accept_uni())
            .await
            .expect("accept_uni timed out")
            .expect("accept_uni failed");
        assert_eq!(first.read_all().await.unwrap().as_ref(), b"");

        // Stream 6 must still be delivered, not dropped as "already closed".
        let mut second = tokio::time::timeout(Duration::from_secs(1), session.accept_uni())
            .await
            .expect("stream 6 was wrongly dropped as already-closed")
            .expect("accept_uni failed");
        assert_eq!(second.read_all().await.unwrap().as_ref(), b"hello");
    }

    /// A RESET_STREAM can be the first frame for a peer-initiated stream. It must
    /// still retire the id, so a later STREAM frame on it isn't accepted as a
    /// brand-new stream. (Guards the RESET-first resurrection path.)
    #[tokio::test]
    async fn reset_as_first_frame_prevents_resurrection() {
        let (session, tx) = scripted_session();

        // RESET arrives before any STREAM frame for this id, then a STREAM does.
        tx.send(uni_reset(5)).unwrap();
        tx.send(uni_stream(5, b"late", false)).unwrap();

        let accepted = tokio::time::timeout(Duration::from_millis(200), session.accept_uni()).await;
        assert!(
            accepted.is_err(),
            "a STREAM after a RESET-first stream resurrected a new accepted stream"
        );
    }
}

// Receive-side DATAGRAM validation. A conforming peer self-limits, so these
// drive a real server `Session` from a hand-crafted raw peer that injects the
// records a conforming client never would.
#[cfg(all(test, feature = "tcp"))]
mod datagram_recv_tests {
    use super::*;
    use crate::transport::Stream;
    use tokio::io::{AsyncWriteExt, DuplexStream};
    use web_transport_trait::Session as _;

    /// Wrap a QMux01 frame in its size-prefixed record — the byte-stream framing
    /// [`Stream`] delimits on the wire.
    fn record(frame: Bytes) -> Bytes {
        let mut buf = bytes::BytesMut::new();
        VarInt::try_from(frame.len()).unwrap().encode(&mut buf);
        buf.extend_from_slice(&frame);
        buf.freeze()
    }

    /// Establish a real server `Session` opposite a raw peer over an in-memory
    /// duplex, returning the server plus the raw write half so the test can inject
    /// arbitrary records. The raw peer sends its `TRANSPORT_PARAMETERS` first, as a
    /// real QMux01 client would, so the server reaches "established".
    async fn raw_peer(server_cfg: Config) -> (Session, DuplexStream) {
        let (server_io, mut raw) = tokio::io::duplex(1024 * 1024);
        let transport = Stream::new(server_io, Version::QMux01, server_cfg.max_record_size);
        let accept = tokio::spawn(Session::accept(transport, server_cfg));

        let client_params = Config::new(Version::QMux01).to_transport_params();
        let params = Frame::TransportParameters(client_params)
            .encode(Version::QMux01)
            .unwrap();
        raw.write_all(&record(params)).await.unwrap();
        raw.flush().await.unwrap();

        let server = accept.await.unwrap().unwrap();
        (server, raw)
    }

    /// A DATAGRAM whose *frame* size (type byte + length varint + payload) exceeds
    /// the advertised `max_datagram_frame_size` is a protocol violation, not
    /// something to silently drop.
    #[tokio::test]
    async fn oversized_frame_closes_session() {
        let mut cfg = Config::new(Version::QMux01);
        cfg.max_datagram_frame_size = 100;
        let (server, mut raw) = raw_peer(cfg).await;

        // Usable payload is 97 (100 - 1 type byte - 2 length-varint bytes); a
        // 98-byte payload tips the encoded frame to 101 > 100.
        let datagram = Frame::Datagram(Bytes::from(vec![0u8; 98]).into())
            .encode(Version::QMux01)
            .unwrap();
        raw.write_all(&record(datagram)).await.unwrap();
        raw.flush().await.unwrap();

        assert!(matches!(server.closed().await, Error::FrameTooLarge));
    }

    /// A DATAGRAM on a session that advertised `max_datagram_frame_size = 0` was
    /// never negotiated; reject the session rather than accept the frame.
    #[tokio::test]
    async fn unnegotiated_datagram_closes_session() {
        let mut cfg = Config::new(Version::QMux01);
        cfg.max_datagram_frame_size = 0;
        let (server, mut raw) = raw_peer(cfg).await;

        let datagram = Frame::Datagram(Bytes::from_static(b"hi").into())
            .encode(Version::QMux01)
            .unwrap();
        raw.write_all(&record(datagram)).await.unwrap();
        raw.flush().await.unwrap();

        assert!(matches!(server.closed().await, Error::DatagramsUnsupported));
    }

    /// A DATAGRAM whose encoded frame exactly hits the advertised limit is
    /// delivered — the bound is inclusive.
    #[tokio::test]
    async fn frame_at_limit_delivered() {
        let mut cfg = Config::new(Version::QMux01);
        cfg.max_datagram_frame_size = 100;
        let (server, mut raw) = raw_peer(cfg).await;

        // 97-byte payload → 1 + 2 + 97 == 100 == the limit.
        let payload = vec![7u8; 97];
        let datagram = Frame::Datagram(Bytes::from(payload.clone()).into())
            .encode(Version::QMux01)
            .unwrap();
        raw.write_all(&record(datagram)).await.unwrap();
        raw.flush().await.unwrap();

        assert_eq!(server.recv_datagram().await.unwrap().as_ref(), &payload[..]);
    }

    /// The no-length (0x30) form carries no length varint, so its frame is only
    /// `1 + payload`. The size check must use that exact size, not the larger
    /// length-prefixed reconstruction — otherwise a conforming 0x30 datagram at
    /// the boundary is wrongly rejected.
    #[tokio::test]
    async fn no_length_datagram_uses_exact_frame_size() {
        let mut cfg = Config::new(Version::QMux01);
        cfg.max_datagram_frame_size = 100;
        let (server, mut raw) = raw_peer(cfg).await;

        // A 99-byte 0x30 payload is a 1 + 99 = 100-byte frame — exactly the limit
        // — even though the length-prefixed reconstruction (1 + 2 + 99 = 102)
        // would overshoot it. We never emit 0x30, so hand-build the frame: a
        // single 0x30 type byte (a 1-byte varint) followed by the payload.
        let payload = vec![3u8; 99];
        let mut frame = bytes::BytesMut::new();
        frame.put_u8(0x30);
        frame.extend_from_slice(&payload);
        raw.write_all(&record(frame.freeze())).await.unwrap();
        raw.flush().await.unwrap();

        assert_eq!(server.recv_datagram().await.unwrap().as_ref(), &payload[..]);
    }
}
