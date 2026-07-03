use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use crate::config::Config;
use crate::credit::Credit;
use crate::sched::PriorityQueue;
use crate::transport::{Transport, TransportReader, TransportWriter};
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

/// How many outbound datagrams to buffer before dropping. The writer pulls from
/// this lane; when it stalls on transport backpressure it stops pulling, the lane
/// fills, and `send_datagram` drops on a full lane. Kept small so shedding tracks
/// real backpressure closely rather than after a deep buffer of stale datagrams.
const DATAGRAM_SEND_BUFFER: usize = 64;

/// Shared, lock-guarded per-stream backend state. The reader task inserts/looks
/// up entries as inbound frames arrive; the writer task retires an entry when it
/// emits that stream's terminal frame (FIN/RESET/STOP_SENDING). Guarded by a
/// plain `std::sync::Mutex` — never held across an `.await` — so both tasks share
/// it without message passing, the way a QUIC endpoint shares connection state.
#[derive(Default)]
struct Streams {
    send: HashMap<StreamId, SendState>,
    recv: HashMap<StreamId, RecvState>,

    // The peer's initial per-stream send-credit limits, applied to the streams
    // we open. Zero until the peer's transport parameters arrive;
    // `recv_transport_parameters` publishes them here under this lock, and
    // `open_uni`/`open_bi` seed a freshly opened stream's credit from them under
    // the same lock. That serialization credits a stream opened concurrently with
    // the handshake exactly once — either here at open time, or by the params
    // handler when it walks the map (whichever takes the lock second sees the
    // other's effect).
    peer_initial_max_stream_data_uni: u64,
    peer_initial_max_stream_data_bidi_remote: u64,
}

/// Closes the connection once the last [`Session`] handle is dropped. Held in an
/// `Arc` cloned with every `Session`, so its `Drop` runs only when they're all
/// gone — at which point it flips `closed`, tearing the backend tasks down
/// promptly rather than waiting for the transport to notice. Mirrors how a QUIC
/// endpoint's connection handle owns the connection's lifetime.
struct SessionGuard {
    closed: watch::Sender<Option<Error>>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.closed.send_if_modified(|slot| {
            if slot.is_none() {
                *slot = Some(Error::Closed);
                true
            } else {
                false
            }
        });
    }
}

/// A multiplexed session over a reliable transport.
#[derive(Clone)]
pub struct Session {
    is_server: bool,
    config: Config,

    outbound: PriorityQueue,
    outbound_priority: mpsc::UnboundedSender<Frame>,

    accept_bi: Arc<tokio::sync::Mutex<mpsc::Receiver<(SendStream, RecvStream)>>>,
    accept_uni: Arc<tokio::sync::Mutex<mpsc::Receiver<RecvStream>>>,

    // Shared per-stream backend state (with the reader and writer tasks). The
    // frontend registers the streams it opens directly under this lock — see
    // `open_uni`/`open_bi` — rather than handing them to the reader over a
    // channel, so the backend exists before the returned stream can enqueue a
    // frame (no open-vs-writer race) and there's no message-passing hop.
    streams: Arc<Mutex<Streams>>,

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

    // Closes the connection when the last `Session` clone drops. Never read.
    _guard: Arc<SessionGuard>,
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

/// Reader-side task state: owns the transport receive half and processes inbound
/// frames. The outbound path (scheduling, encoding, sending, keep-alive) lives in
/// [`WriterState`]; the two tasks share `streams` and the record-limit / idle
/// atomics instead of passing messages.
struct SessionState<R: TransportReader> {
    reader: R,
    config: Config,
    is_server: bool,

    // Handed (cloned) to newly-created peer-initiated stream frontends so they can
    // enqueue their own data (`outbound`) and control frames (`control`). The
    // reader never pulls from these — the writer does.
    outbound: PriorityQueue,
    control: mpsc::UnboundedSender<Frame>,

    accept_bi: mpsc::Sender<(SendStream, RecvStream)>,
    accept_uni: mpsc::Sender<RecvStream>,

    // Shared per-stream backend state (with the frontend and writer). The
    // frontend inserts streams it opens; the reader inserts peer-initiated ones.
    streams: Arc<Mutex<Streams>>,

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
    // brand-new accepted stream. See `RecvOpen`. Reader-only (not shared with the
    // writer). QMux only: MAX_STREAMS flow control bounds the hole set to at most
    // the peer's stream limit.
    recv_open_bi: RecvOpen,
    recv_open_uni: RecvOpen,

    // QMux01 idle-timeout state: the reader closes the session if no frame arrives
    // within the window (the writer handles the keep-alive ping side).
    last_recv_at: tokio::time::Instant,

    // Inbound datagram sink (see the matching field on `Session`) plus the
    // shared send-limit cell resolved from the peer's params.
    recv_datagram: mpsc::Sender<Bytes>,
    datagram_max_size: Arc<AtomicUsize>,

    // Effective outbound record-size limit and idle-timeout (ms), shared with the
    // writer. Both are written once, when the peer's transport parameters arrive.
    record_limit: Arc<AtomicU64>,
    idle_timeout_ms: Arc<AtomicU64>,

    // Set by the writer while a `send` is in flight (see `WriterState`). The reader
    // consults it before firing the idle timeout, so transport backpressure isn't
    // mistaken for a dead peer.
    writer_backpressured: Arc<AtomicBool>,

    // When the reader started deferring the idle timeout because the writer was
    // backpressured, or `None` if it isn't currently deferring. Bounds the deferral:
    // backpressure buys the connection at most one extra idle window before it's
    // reclaimed anyway, so a peer that dies with our send buffer full is still
    // idle-closed (just later) rather than hanging until the transport times out.
    // Reset on any receive.
    backpressure_suppressed_since: Option<tokio::time::Instant>,
}

/// Pick the next outbound frame in strict priority order: control (lossless,
/// e.g. RESET/STOP/CLOSE/window updates) first, then datagrams (low-latency but
/// droppable), then bulk stream data scheduled by [`PriorityQueue`]. Returns
/// `None` only once the stream queue is closed, which drives session teardown.
///
/// Each source's future is cancel-safe (`mpsc::recv` and `PriorityQueue::pop`
/// remove nothing until they resolve), so losing this race in the caller's
/// `select!` never drops a frame.
async fn next_outbound(
    control: &mut mpsc::UnboundedReceiver<Frame>,
    datagram: &mut mpsc::Receiver<Bytes>,
    stream: &PriorityQueue,
) -> Option<Frame> {
    tokio::select! {
        biased;
        Some(frame) = control.recv() => Some(frame),
        // `.into()` builds the length-prefixed (0x31) form we always emit.
        Some(payload) = datagram.recv() => Some(Frame::Datagram(payload.into())),
        frame = stream.pop() => frame,
    }
}

/// RFC 9000 §10.1 effective idle timeout in ms: the smaller of the two advertised
/// values, ignoring a zero (disabled) side. Returns 0 when both are disabled.
/// Shared by the reader's idle deadline and the writer's keep-alive cadence so the
/// two never drift.
fn negotiated_idle_timeout_ms(ours: u64, peer: u64) -> u64 {
    match (ours, peer) {
        (0, 0) => 0,
        (a, 0) | (0, a) => a,
        (a, b) => a.min(b),
    }
}

/// Writer-side task state: owns the transport send half and is the sole producer
/// on the wire. It pulls the outbound queues in strict priority order via
/// [`next_outbound`], retires the stream a terminal frame closes and encodes it
/// under the shared `streams` lock, then writes it. Runs on its own task so a
/// write blocked on transport backpressure never stalls the reader. It also owns
/// the QMux keep-alive ping (it's the side that knows when we last sent).
struct WriterState<W: TransportWriter> {
    writer: W,
    version: Version,

    control: mpsc::UnboundedReceiver<Frame>,
    datagrams: mpsc::Receiver<Bytes>,
    outbound: PriorityQueue,

    // Shared with the reader task.
    streams: Arc<Mutex<Streams>>,
    record_limit: Arc<AtomicU64>,
    idle_timeout_ms: Arc<AtomicU64>,

    // Set while a `send` is in flight so the reader can tell a wedged-on-
    // backpressure connection (peer alive, its recv window full) apart from a
    // genuinely dead one, and not idle-close the former. See `transmit`.
    writer_backpressured: Arc<AtomicBool>,

    closed: watch::Sender<Option<Error>>,

    last_send_at: tokio::time::Instant,
    next_ping_seq: u64,
}

impl<W: TransportWriter> WriterState<W> {
    /// Record the first terminal error so the reader's `closed` branch unblocks.
    fn note_closed(&self, err: Error) {
        self.closed.send_if_modified(|slot| {
            if slot.is_none() {
                *slot = Some(err);
                true
            } else {
                false
            }
        });
    }

    async fn run(&mut self) {
        let mut closed_rx = self.closed.subscribe();
        loop {
            // Keep-alive: send a QX_PING once we've been silent for a third of the
            // idle timeout. 0 = disabled (non-QMux01, or params not yet exchanged).
            // Clamp to 1ms so a tiny timeout doesn't yield a zero-duration deadline.
            let idle_ms = self.idle_timeout_ms.load(Ordering::Acquire);
            let ping_deadline = (idle_ms != 0).then(|| {
                self.last_send_at + std::time::Duration::from_millis((idle_ms / 3).max(1))
            });

            tokio::select! {
                biased;
                frame = next_outbound(&mut self.control, &mut self.datagrams, &self.outbound) => {
                    match frame {
                        Some(frame) => {
                            if let Err(err) = self.transmit(frame).await {
                                self.note_closed(err);
                                break;
                            }
                        }
                        // The stream queue was closed on teardown.
                        None => break,
                    }
                }
                _ = async { tokio::time::sleep_until(ping_deadline.unwrap()).await }, if ping_deadline.is_some() => {
                    let seq = self.next_ping_seq;
                    self.next_ping_seq = self.next_ping_seq.wrapping_add(1);
                    let ping = Frame::Ping(crate::Ping { sequence: seq, response: false });
                    if let Err(err) = self.transmit(ping).await {
                        self.note_closed(err);
                        break;
                    }
                }
                // Transport-level maintenance (WebSocket keep-alive Ping); never
                // resolves for transports without timer-driven work.
                result = self.writer.maintain() => {
                    if let Err(err) = result {
                        self.note_closed(err);
                        break;
                    }
                }
                // Wrapped so the `watch::Ref` guard is dropped before the branch
                // resolves — otherwise it (non-`Send`), held across a `send` await,
                // would make the task non-`Send`.
                _ = async { closed_rx.wait_for(|slot| slot.is_some()).await.ok(); } => {
                    // Session tearing down: best-effort flush of any queued control
                    // frames (e.g. a ConnectionClose) before we stop.
                    while let Ok(frame) = self.control.try_recv() {
                        if self.transmit(frame).await.is_err() {
                            break;
                        }
                    }
                    break;
                }
            }
        }
        let _ = self.writer.close().await;
    }

    /// Retire the stream a terminal frame closes, encode the frame (validating its
    /// size for QMux01), and write it. The `streams` lock is only held for the
    /// synchronous retirement, never across the `send` await.
    async fn transmit(&mut self, frame: Frame) -> Result<(), Error> {
        match &frame {
            Frame::ResetStream(reset) => {
                self.streams.lock().unwrap().send.remove(&reset.id);
            }
            Frame::Stream(stream) if stream.fin => {
                self.streams.lock().unwrap().send.remove(&stream.id);
            }
            Frame::StopSending(stop) => {
                self.streams.lock().unwrap().recv.remove(&stop.id);
            }
            _ => {}
        }

        let bytes = frame.encode(self.version)?;
        if self.version == Version::QMux01 {
            // `record_limit` holds the draft-01 default until the peer's params
            // arrive, then the peer's `max_record_size`.
            let limit = self.record_limit.load(Ordering::Acquire);
            if bytes.len() as u64 > limit {
                return Err(Error::FrameTooLarge);
            }
        }
        // Flag the in-flight write so the reader won't idle-close a connection
        // that's merely backpressured: a `send` stuck here proves the peer is
        // still there (its receive window is just full). Cleared as soon as the
        // write lands. Only the session idle timeout consults this — a WebSocket
        // transport's own keep-alive deadline is independent.
        self.writer_backpressured.store(true, Ordering::Release);
        let result = self.writer.send(bytes).await;
        self.writer_backpressured.store(false, Ordering::Release);
        result?;
        self.last_send_at = tokio::time::Instant::now();
        Ok(())
    }
}

impl<R: TransportReader> SessionState<R> {
    async fn run(&mut self) -> Result<(), Error> {
        let mut closed = self.closed.subscribe();

        loop {
            // Close the session if the peer goes silent past the idle timeout. The
            // writer owns the keep-alive ping that keeps this from firing while a
            // healthy peer is merely idle. Disabled until the params are exchanged.
            let idle_deadline = self
                .effective_idle_timeout_ms()
                .map(|ms| self.last_recv_at + std::time::Duration::from_millis(ms));

            tokio::select! {
                biased;
                result = self.reader.recv() => {
                    let data = result?;
                    self.last_recv_at = tokio::time::Instant::now();
                    // Real progress ends any backpressure deferral window.
                    self.backpressure_suppressed_since = None;
                    if self.config.version == Version::QMux01 {
                        // QMux01: data is a record containing one or more frames
                        for frame in Frame::decode_record(data)? {
                            self.recv_frame(frame).await?;
                        }
                    } else if let Some(frame) = Frame::decode(data, self.config.version)? {
                        self.recv_frame(frame).await?;
                    }
                }
                _ = async { tokio::time::sleep_until(idle_deadline.unwrap()).await }, if idle_deadline.is_some() => {
                    let now = tokio::time::Instant::now();
                    // One idle window of grace (the deadline is armed, so this is Some).
                    let grace = std::time::Duration::from_millis(
                        self.effective_idle_timeout_ms().unwrap_or(0),
                    );
                    match self.backpressure_suppressed_since {
                        // Already deferring: keep the connection alive until the grace
                        // window elapses, then reclaim it even if still backpressured —
                        // a peer that died with our send buffer full must not hang here.
                        Some(since) if now.duration_since(since) < grace => {
                            self.last_recv_at = now; // re-arm the deadline; avoid a busy spin
                            continue;
                        }
                        // Grace exhausted — fall through and idle-close.
                        Some(_) => {}
                        None => {
                            if self.writer_backpressured.load(Ordering::Acquire) {
                                // Writer wedged on transport backpressure: the peer's
                                // receive window is full, which is evidence it's alive
                                // and that we simply can't get a keep-alive out. Defer
                                // the close, but only for the bounded grace above.
                                self.backpressure_suppressed_since = Some(now);
                                self.last_recv_at = now;
                                continue;
                            }
                        }
                    }
                    tracing::debug!("idle timeout fired");
                    return Err(Error::IdleTimeout);
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
        match negotiated_idle_timeout_ms(
            self.our_params.max_idle_timeout,
            self.peer_params.max_idle_timeout,
        ) {
            0 => None,
            ms => Some(ms),
        }
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

                // Ignore a post-terminal frame on a retired peer-initiated stream
                // before consuming connection credit — otherwise a flood of
                // duplicate/late frames would drain conn flow-control that's never
                // replenished (they're not delivered). `is_closed` distinguishes a
                // retired id from one merely implicitly opened (a higher index
                // arrived first); a live stream is delivered by the fast path below,
                // so exclude it. Only QMux tracks this (MAX_STREAMS bounds the holes).
                let live = self.streams.lock().unwrap().recv.contains_key(&stream.id);
                if self.config.version.is_qmux()
                    && stream.id.server_initiated() != self.is_server
                    && !live
                    && self.recv_open(stream.id.dir()).is_closed(stream.id.index())
                {
                    return Ok(());
                }

                // Connection-level flow control.
                let data_len = stream.data.len() as u64;
                if data_len > 0 && !self.conn_recv_credit.receive(data_len) {
                    return Err(Error::FlowControlError);
                }

                // Fast path: an existing stream. Check its window and deliver under
                // a brief lock (never held across an await).
                {
                    let mut streams = self.streams.lock().unwrap();
                    if let Some(recv) = streams.recv.get(&stream.id) {
                        if data_len > 0 && !recv.recv_credit.receive(data_len) {
                            return Err(Error::FlowControlError);
                        }
                        let id = stream.id;
                        let fin = stream.fin;
                        recv.inbound_data.send(stream).ok();
                        if fin {
                            streams.recv.remove(&id);
                        }
                        return Ok(());
                    }
                }

                // A frame on one of our own (already-retired) streams: ignore it.
                if self.is_server == stream.id.server_initiated() {
                    return Ok(());
                }

                // New peer-initiated stream. Enforce the stream-count limit — per
                // RFC 9000 §4.6 opening index N implicitly opens all of 0..N.
                if self.config.version.is_qmux() {
                    let credit = match stream.id.dir() {
                        StreamDir::Bi => &self.recv_bi_credit,
                        StreamDir::Uni => &self.recv_uni_credit,
                    };
                    if !credit.receive_up_to(stream.id.index() + 1) {
                        return Err(Error::StreamLimitExceeded);
                    }

                    // Record that we've instantiated a frontend for this id, so a
                    // later frame on it (once retired) reads as closed rather than
                    // resurrecting a new stream. After the credit gate, which bounds
                    // the hole set to MAX_STREAMS.
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
                        StreamDir::Bi => self.our_params.initial_max_stream_data_bidi_remote,
                        StreamDir::Uni => self.our_params.initial_max_stream_data_uni,
                    }
                } else {
                    u64::MAX
                };

                let recv_credit = Credit::new(recv_window);

                // Stream-level flow control for the first frame on the new stream.
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
                    outbound_priority: self.control.clone(),
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
                            outbound_priority: self.control.clone(),
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

                        self.streams
                            .lock()
                            .unwrap()
                            .send
                            .insert(stream.id, send_backend);
                        self.accept_bi
                            .send((send_frontend, recv_frontend))
                            .await
                            .map_err(|_| Error::Closed)?;
                    }
                };

                let id = stream.id;
                let fin = stream.fin;
                recv_backend.inbound_data.send(stream).ok();

                if !fin {
                    self.streams.lock().unwrap().recv.insert(id, recv_backend);
                }
            }
            Frame::ResetStream(reset) => {
                if !reset.id.can_recv(self.is_server) {
                    return Err(Error::InvalidStreamId);
                }

                // Live stream: deliver the reset and drop it (it was recorded in
                // `recv_open` at creation, so it now reads as closed).
                let reset_id = reset.id;
                let delivered = {
                    let mut streams = self.streams.lock().unwrap();
                    if let Some(recv) = streams.recv.remove(&reset_id) {
                        recv.inbound_reset.send(reset).ok();
                        true
                    } else {
                        false
                    }
                };
                if !delivered
                    && self.config.version.is_qmux()
                    && reset_id.server_initiated() != self.is_server
                {
                    // RESET_STREAM can be the *first* frame for a peer-initiated
                    // stream (it implicitly opens the id). Record it as closed so a
                    // later STREAM on the same id is recognized as retired rather than
                    // resurrected into a new accepted stream. Gate on the stream limit
                    // first (mirroring the STREAM path) so the hole set stays bounded
                    // by MAX_STREAMS.
                    let credit = match reset_id.dir() {
                        StreamDir::Bi => &self.recv_bi_credit,
                        StreamDir::Uni => &self.recv_uni_credit,
                    };
                    if !credit.receive_up_to(reset_id.index() + 1) {
                        return Err(Error::StreamLimitExceeded);
                    }
                    match reset_id.dir() {
                        StreamDir::Bi => &mut self.recv_open_bi,
                        StreamDir::Uni => &mut self.recv_open_uni,
                    }
                    .record(reset_id.index());
                }
            }
            Frame::StopSending(stop) => {
                if !stop.id.can_send(self.is_server) {
                    return Err(Error::InvalidStreamId);
                }

                if let Some(send) = self.streams.lock().unwrap().send.get(&stop.id) {
                    send.inbound_stopped.send(stop).ok();
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
                if let Some(send) = self.streams.lock().unwrap().send.get(&id) {
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
                    self.control.send(response).ok();
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

        // Publish the peer's initial per-stream send limits and credit the streams
        // we've already opened — both under one lock, so a stream being opened
        // concurrently is credited exactly once: either it's already in the map and
        // this walk credits it, or it's not yet inserted and `open_uni`/`open_bi`
        // reads the values we just published and credits itself.
        {
            let mut streams = self.streams.lock().unwrap();
            streams.peer_initial_max_stream_data_uni = params.initial_max_stream_data_uni;
            streams.peer_initial_max_stream_data_bidi_remote =
                params.initial_max_stream_data_bidi_remote;
            for (id, send) in &streams.send {
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
        }

        // Publish the two scalars the writer task needs, now that they're known:
        // the outbound record-size limit (QMux01 only) and the effective idle
        // timeout for its keep-alive ping. `record_limit` was seeded with the
        // draft-01 default; raise it to the peer's advertised size.
        let idle_ms = if self.config.version == Version::QMux01 {
            self.record_limit
                .store(params.max_record_size, Ordering::Release);
            negotiated_idle_timeout_ms(self.our_params.max_idle_timeout, params.max_idle_timeout)
        } else {
            0
        };
        self.idle_timeout_ms.store(idle_ms, Ordering::Release);

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

        let outbound = PriorityQueue::new(8);
        // Control lane (lossless): RESET/STOP/CLOSE, window updates, pings, and the
        // initial TRANSPORT_PARAMETERS. The reader and stream frontends produce;
        // the writer consumes.
        let (control_tx, control_rx) = mpsc::unbounded_channel();

        // Bounded, lossy datagram channels — drop on a full buffer rather than
        // stalling, matching QUIC's unreliable semantics. When the writer stalls on
        // backpressure it stops draining `outbound_datagram`, which fills and makes
        // `send_datagram` shed.
        let (recv_datagram_tx, recv_datagram_rx) = mpsc::channel(DATAGRAM_RECV_BUFFER);
        let (outbound_datagram_tx, outbound_datagram_rx) = mpsc::channel(DATAGRAM_SEND_BUFFER);
        let datagram_max_size = Arc::new(AtomicUsize::new(0));

        // Shared with the writer task: per-stream backend state, plus the two
        // scalars the writer needs — the outbound record-size limit (QMux01 seeds
        // it with the draft-01 default) and the effective idle timeout for its
        // keep-alive ping (0 until the peer's params arrive).
        let streams: Arc<Mutex<Streams>> = Arc::new(Mutex::new(Streams::default()));
        let record_limit = Arc::new(AtomicU64::new(crate::proto::DEFAULT_MAX_RECORD_SIZE));
        let idle_timeout_ms = Arc::new(AtomicU64::new(0));
        // True while the writer is blocked in a `send`; lets the reader distinguish
        // transport backpressure from a dead peer when the idle timeout fires.
        let writer_backpressured = Arc::new(AtomicBool::new(false));

        let closed = watch::Sender::new(None);

        // The QMux handshake requires TRANSPORT_PARAMETERS as the first frame. It
        // leads the FIFO control lane, so the writer emits it before anything else.
        if version.is_qmux() {
            control_tx
                .send(Frame::TransportParameters(our_params.clone()))
                .ok();
        }

        // Split the transport into halves driven by two tasks: a write blocked on
        // backpressure must never stall reads. The writer is the sole producer on
        // the wire, pulling the outbound queues in priority order and sharing the
        // stream maps + scalars above with the reader (no message-passing handoff).
        let (writer_half, reader_half) = transport.split();
        let mut writer = WriterState {
            writer: writer_half,
            version,
            control: control_rx,
            datagrams: outbound_datagram_rx,
            outbound: outbound.clone(),
            streams: streams.clone(),
            record_limit: record_limit.clone(),
            idle_timeout_ms: idle_timeout_ms.clone(),
            writer_backpressured: writer_backpressured.clone(),
            closed: closed.clone(),
            last_send_at: tokio::time::Instant::now(),
            next_ping_seq: 0,
        };
        tokio::spawn(async move { writer.run().await });

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
            reader: reader_half,
            config: config.clone(),
            is_server,
            outbound: outbound.clone(),
            control: control_tx.clone(),
            accept_bi: accept_bi_tx,
            accept_uni: accept_uni_tx,
            streams: streams.clone(),
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
            recv_datagram: recv_datagram_tx,
            datagram_max_size: datagram_max_size.clone(),
            record_limit: record_limit.clone(),
            idle_timeout_ms: idle_timeout_ms.clone(),
            writer_backpressured,
            backpressure_suppressed_since: None,
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
            for send in backend.streams.lock().unwrap().send.values() {
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

        // Closes the connection once every `Session` clone has dropped.
        let guard = Arc::new(SessionGuard {
            closed: closed.clone(),
        });

        Session {
            is_server,
            config,
            outbound,
            outbound_priority: control_tx,
            accept_bi: Arc::new(tokio::sync::Mutex::new(accept_bi_rx)),
            accept_uni: Arc::new(tokio::sync::Mutex::new(accept_uni_rx)),
            streams,
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
            _guard: guard,
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

        // Register the backend before returning the frontend, so the stream exists
        // in the shared map before it can enqueue a frame. Seed its send credit
        // from the peer's params if they've already arrived (otherwise it's still
        // zero here and `recv_transport_parameters` will credit it later) — see the
        // note on `Streams::peer_initial_max_stream_data_uni`.
        {
            let mut streams = self.streams.lock().unwrap();
            if let Some(credit) = &send_backend.stream_credit {
                credit
                    .increase_max(streams.peer_initial_max_stream_data_uni)
                    .ok();
            }
            streams.send.insert(id, send_backend);
        }

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

        // Register both backends before returning the frontends (see `open_uni`).
        // A bidi stream we initiate sends under the peer's `bidi_remote` limit.
        {
            let mut streams = self.streams.lock().unwrap();
            if let Some(credit) = &send_backend.stream_credit {
                credit
                    .increase_max(streams.peer_initial_max_stream_data_bidi_remote)
                    .ok();
            }
            streams.send.insert(id, send_backend);
            streams.recv.insert(id, recv_backend);
        }

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
        // contract. When the writer stalls on transport backpressure it stops
        // draining this lane, so a full lane *is* the backpressure signal: shed the
        // datagram (returning `Ok` — an unreliable datagram is meant to be
        // droppable) rather than block or grow without bound. A closed lane means
        // the session is gone.
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

    use super::{Session, Transport, TransportReader, TransportWriter};
    use crate::proto::{Frame, ResetStream, Stream};
    use crate::{Config, Error, StreamDir, StreamId, Version};

    /// A transport whose inbound frames are scripted through a channel; outbound
    /// writes are discarded. Once the script is drained, `recv` parks forever so
    /// the session's run loop keeps running (rather than seeing a closed
    /// transport and tearing down).
    struct ScriptedTransport {
        incoming: mpsc::UnboundedReceiver<Bytes>,
    }

    struct ScriptedWriter;

    struct ScriptedReader {
        incoming: mpsc::UnboundedReceiver<Bytes>,
    }

    impl Transport for ScriptedTransport {
        type Writer = ScriptedWriter;
        type Reader = ScriptedReader;

        fn split(self) -> (ScriptedWriter, ScriptedReader) {
            (
                ScriptedWriter,
                ScriptedReader {
                    incoming: self.incoming,
                },
            )
        }
    }

    impl TransportWriter for ScriptedWriter {
        async fn send(&mut self, _data: Bytes) -> Result<(), Error> {
            Ok(())
        }

        async fn close(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

    impl TransportReader for ScriptedReader {
        async fn recv(&mut self) -> Result<Bytes, Error> {
            match self.incoming.recv().await {
                Some(bytes) => Ok(bytes),
                None => std::future::pending().await,
            }
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
