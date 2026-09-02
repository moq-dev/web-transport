//! A sans-I/O transport implementing the poll traits, driven with no runtime.
//!
//! This is the acceptance test for the poll surface: if a transport like this cannot
//! be written against these traits, the traits are wrong. Specifically it pins three
//! properties that are easy to lose in a refactor.
//!
//! 1. **It is `!Send`.** Every type here holds an [`Rc`]. A `Send` bound anywhere on
//!    [`Session`], [`SendStream`], [`RecvStream`] — or on the associated
//!    stream types, which is the subtle way it creeps back in — would reject this
//!    file at compile time. `requires_*` below force the bounds to be checked.
//!
//! 2. **It needs no `Arc<Mutex<..>>`.** The session owns its state and mutates it
//!    through `&mut self`. The `RefCell`s here are the *wire* between the two peers,
//!    standing in for a network; they are not a lock around a session handle.
//!
//! 3. **Nothing drives it but the test.** No executor, no futures, no `.await`, no
//!    timer. The test steps `poll_*` by hand with a plain [`Waker`], which is what a
//!    caller like a thread-per-core `io_uring` loop would do.

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use bytes::Bytes;
use web_transport_trait::poll::{RecvStream, SendStream, Session};

// --- error --------------------------------------------------------------------

#[derive(Debug)]
struct LocalError;

impl std::fmt::Display for LocalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "local transport error")
    }
}

impl std::error::Error for LocalError {}

impl web_transport_trait::Error for LocalError {
    fn session_error(&self) -> Option<(u32, String)> {
        None
    }
}

// --- the wire -----------------------------------------------------------------

/// One direction of a stream: bytes plus the terminal flags.
#[derive(Default)]
struct StreamBuf {
    data: VecDeque<u8>,
    fin: bool,
    reset: bool,
    wakers: Vec<Waker>,
}

impl StreamBuf {
    fn wake(&mut self) {
        for waker in self.wakers.drain(..) {
            waker.wake();
        }
    }

    fn park(&mut self, cx: &Context<'_>) {
        if !self.wakers.iter().any(|w| w.will_wake(cx.waker())) {
            self.wakers.push(cx.waker().clone());
        }
    }
}

type Shared = Rc<RefCell<StreamBuf>>;

/// A peer's inbox. Standing in for the network, not for a session lock.
#[derive(Default)]
struct Inbox {
    uni: VecDeque<Shared>,
    bi: VecDeque<(Shared, Shared)>,
    datagrams: VecDeque<Bytes>,
    closed: bool,
    wakers: Vec<Waker>,
}

impl Inbox {
    fn wake(&mut self) {
        for waker in self.wakers.drain(..) {
            waker.wake();
        }
    }

    fn park(&mut self, cx: &Context<'_>) {
        if !self.wakers.iter().any(|w| w.will_wake(cx.waker())) {
            self.wakers.push(cx.waker().clone());
        }
    }
}

// --- streams ------------------------------------------------------------------

/// `!Send`: holds an [`Rc`].
struct LocalSend {
    buf: Shared,
    priority: i32,
}

impl SendStream for LocalSend {
    type Error = LocalError;

    fn poll_write(&mut self, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, LocalError>> {
        let mut state = self.buf.borrow_mut();
        if state.reset {
            return Poll::Ready(Err(LocalError));
        }
        state.data.extend(buf.iter().copied());
        state.wake();
        Poll::Ready(Ok(buf.len()))
    }

    fn set_priority(&mut self, order: i32) {
        self.priority = order;
    }

    fn finish(&mut self) -> Result<(), LocalError> {
        let mut state = self.buf.borrow_mut();
        state.fin = true;
        state.wake();
        Ok(())
    }

    fn reset(&mut self, _code: u32) {
        let mut state = self.buf.borrow_mut();
        state.reset = true;
        state.wake();
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), LocalError>> {
        let mut state = self.buf.borrow_mut();
        if state.reset || (state.fin && state.data.is_empty()) {
            return Poll::Ready(Ok(()));
        }
        state.park(cx);
        Poll::Pending
    }
}

/// `!Send`: holds an [`Rc`].
struct LocalRecv {
    buf: Shared,
}

impl RecvStream for LocalRecv {
    type Error = LocalError;

    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, LocalError>> {
        let mut state = self.buf.borrow_mut();
        if state.reset {
            return Poll::Ready(Err(LocalError));
        }
        if dst.is_empty() {
            // Asking for no bytes is not end of stream.
            return Poll::Ready(Ok(Some(0)));
        }

        let size = dst.len().min(state.data.len());
        if size == 0 {
            if state.fin {
                return Poll::Ready(Ok(None));
            }
            state.park(cx);
            return Poll::Pending;
        }

        for slot in dst.iter_mut().take(size) {
            *slot = state.data.pop_front().expect("checked above");
        }
        Poll::Ready(Ok(Some(size)))
    }

    fn stop(&mut self, _code: u32) {
        let mut state = self.buf.borrow_mut();
        state.reset = true;
        state.wake();
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), LocalError>> {
        let mut state = self.buf.borrow_mut();
        if state.reset || (state.fin && state.data.is_empty()) {
            return Poll::Ready(Ok(()));
        }
        state.park(cx);
        Poll::Pending
    }
}

// --- session ------------------------------------------------------------------

/// `!Send`: holds [`Rc`]s. Not `Clone`, which the traits no longer require.
struct LocalSession {
    /// What we write into: the peer's inbox.
    tx: Rc<RefCell<Inbox>>,
    /// What we read from: our own inbox.
    rx: Rc<RefCell<Inbox>>,
}

impl LocalSession {
    /// A connected pair sharing two inboxes.
    fn pair() -> (Self, Self) {
        let a = Rc::new(RefCell::new(Inbox::default()));
        let b = Rc::new(RefCell::new(Inbox::default()));

        let client = Self {
            tx: b.clone(),
            rx: a.clone(),
        };
        let server = Self { tx: a, rx: b };

        (client, server)
    }
}

impl Session for LocalSession {
    type SendStream = LocalSend;
    type RecvStream = LocalRecv;
    type Error = LocalError;

    fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<LocalRecv, LocalError>> {
        let mut inbox = self.rx.borrow_mut();
        match inbox.uni.pop_front() {
            Some(buf) => Poll::Ready(Ok(LocalRecv { buf })),
            None if inbox.closed => Poll::Ready(Err(LocalError)),
            None => {
                inbox.park(cx);
                Poll::Pending
            }
        }
    }

    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(LocalSend, LocalRecv), LocalError>> {
        let mut inbox = self.rx.borrow_mut();
        match inbox.bi.pop_front() {
            Some((send, recv)) => Poll::Ready(Ok((
                LocalSend {
                    buf: send,
                    priority: 0,
                },
                LocalRecv { buf: recv },
            ))),
            None if inbox.closed => Poll::Ready(Err(LocalError)),
            None => {
                inbox.park(cx);
                Poll::Pending
            }
        }
    }

    fn poll_open_uni(&mut self, _cx: &mut Context<'_>) -> Poll<Result<LocalSend, LocalError>> {
        let buf: Shared = Rc::new(RefCell::new(StreamBuf::default()));
        let mut inbox = self.tx.borrow_mut();
        if inbox.closed {
            return Poll::Ready(Err(LocalError));
        }
        inbox.uni.push_back(buf.clone());
        inbox.wake();
        Poll::Ready(Ok(LocalSend { buf, priority: 0 }))
    }

    fn poll_open_bi(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(LocalSend, LocalRecv), LocalError>> {
        // Our send is the peer's recv, and vice versa.
        let ours: Shared = Rc::new(RefCell::new(StreamBuf::default()));
        let theirs: Shared = Rc::new(RefCell::new(StreamBuf::default()));

        let mut inbox = self.tx.borrow_mut();
        if inbox.closed {
            return Poll::Ready(Err(LocalError));
        }
        inbox.bi.push_back((theirs.clone(), ours.clone()));
        inbox.wake();

        Poll::Ready(Ok((
            LocalSend {
                buf: ours,
                priority: 0,
            },
            LocalRecv { buf: theirs },
        )))
    }

    fn poll_send_datagram(
        &mut self,
        cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), LocalError>> {
        let mut inbox = self.tx.borrow_mut();
        if inbox.closed {
            return Poll::Ready(Err(LocalError));
        }
        // A tiny queue, so the backpressure path is exercised rather than
        // theoretical.
        if inbox.datagrams.len() >= 2 {
            inbox.park(cx);
            return Poll::Pending;
        }
        inbox.datagrams.push_back(Bytes::copy_from_slice(payload));
        inbox.wake();
        Poll::Ready(Ok(()))
    }

    fn protocol(&self) -> Option<&str> {
        None
    }

    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, LocalError>> {
        let mut inbox = self.rx.borrow_mut();
        match inbox.datagrams.pop_front() {
            Some(datagram) => Poll::Ready(Ok(datagram)),
            None if inbox.closed => Poll::Ready(Err(LocalError)),
            None => {
                inbox.park(cx);
                Poll::Pending
            }
        }
    }

    fn max_datagram_size(&self) -> usize {
        1200
    }

    fn stats(&self) -> impl web_transport_trait::Stats {
        web_transport_trait::StatsUnavailable
    }

    fn close(&mut self, _code: u32, _reason: &str) {
        for inbox in [&self.tx, &self.rx] {
            let mut inbox = inbox.borrow_mut();
            inbox.closed = true;
            inbox.wake();
        }
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<LocalError> {
        let mut inbox = self.rx.borrow_mut();
        if inbox.closed {
            return Poll::Ready(LocalError);
        }
        inbox.park(cx);
        Poll::Pending
    }
}

// --- the bounds themselves ----------------------------------------------------

/// These would not compile if the poll traits carried a `Send` bound, or if
/// `Session` required `Send` of its associated stream types.
fn requires_poll_session<S: Session>() {}
fn requires_poll_send_stream<S: SendStream>() {}
fn requires_poll_recv_stream<S: RecvStream>() {}

#[test]
fn a_not_send_transport_implements_the_poll_surface() {
    requires_poll_session::<LocalSession>();
    requires_poll_send_stream::<LocalSend>();
    requires_poll_recv_stream::<LocalRecv>();
}

// --- driving it, with no runtime ----------------------------------------------

struct Noop;

impl Wake for Noop {
    fn wake(self: Arc<Self>) {}
}

fn ready<T>(poll: Poll<T>) -> T {
    match poll {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("expected Ready"),
    }
}

#[test]
fn a_uni_stream_moves_bytes_without_an_executor() {
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);

    let (mut client, mut server) = LocalSession::pair();

    // Nothing has been opened, so the server parks.
    assert!(server.poll_accept_uni(&mut cx).is_pending());

    let mut send = ready(client.poll_open_uni(&mut cx)).unwrap();
    assert_eq!(ready(send.poll_write(&mut cx, b"hello")).unwrap(), 5);
    send.finish().unwrap();

    let mut recv = ready(server.poll_accept_uni(&mut cx)).unwrap();

    let mut dst = [0u8; 16];
    let size = ready(recv.poll_read(&mut cx, &mut dst)).unwrap().unwrap();
    assert_eq!(&dst[..size], b"hello");

    // Drained and finished: end of stream, and both halves agree it is closed.
    assert_eq!(ready(recv.poll_read(&mut cx, &mut dst)).unwrap(), None);
    ready(recv.poll_closed(&mut cx)).unwrap();
    ready(send.poll_closed(&mut cx)).unwrap();
}

#[test]
fn a_bi_stream_moves_bytes_in_both_directions() {
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);

    let (mut client, mut server) = LocalSession::pair();

    let (mut client_send, mut client_recv) = ready(client.poll_open_bi(&mut cx)).unwrap();
    ready(client_send.poll_write(&mut cx, b"ping")).unwrap();

    let (mut server_send, mut server_recv) = ready(server.poll_accept_bi(&mut cx)).unwrap();

    let mut dst = [0u8; 16];
    let size = ready(server_recv.poll_read(&mut cx, &mut dst))
        .unwrap()
        .unwrap();
    assert_eq!(&dst[..size], b"ping");

    ready(server_send.poll_write(&mut cx, b"pong")).unwrap();
    let size = ready(client_recv.poll_read(&mut cx, &mut dst))
        .unwrap()
        .unwrap();
    assert_eq!(&dst[..size], b"pong");
}

#[test]
fn datagrams_round_trip() {
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);

    let (mut client, mut server) = LocalSession::pair();

    assert!(server.poll_recv_datagram(&mut cx).is_pending());

    let payload = Bytes::from_static(b"dgram");
    ready(client.poll_send_datagram(&mut cx, &payload)).unwrap();
    let datagram = ready(server.poll_recv_datagram(&mut cx)).unwrap();
    assert_eq!(&datagram[..], b"dgram");
}

/// The provided `poll_read_buf`/`poll_read_chunk` work against a bare
/// implementation that only wrote `poll_read`.
#[test]
fn the_provided_read_helpers_work_over_poll_read() {
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);

    let (mut client, mut server) = LocalSession::pair();

    let mut send = ready(client.poll_open_uni(&mut cx)).unwrap();
    ready(send.poll_write(&mut cx, b"chunky")).unwrap();
    send.finish().unwrap();

    let mut recv = ready(server.poll_accept_uni(&mut cx)).unwrap();

    let chunk = ready(recv.poll_read_chunk(&mut cx, 4)).unwrap().unwrap();
    assert_eq!(&chunk[..], b"chun");
    assert!(chunk.len() <= 4, "poll_read_chunk exceeded max");

    let mut buf = bytes::BytesMut::with_capacity(16);
    let size = ready(recv.poll_read_buf(&mut cx, &mut buf))
        .unwrap()
        .unwrap();
    assert_eq!(size, 2);
    assert_eq!(&buf[..], b"ky");
}

/// A caller with no room left is not at end of stream. Backends distinguish the two
/// and the defaults must not collapse them into `None`, which reads as truncation.
#[test]
fn a_full_destination_is_not_end_of_stream() {
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);

    let (mut client, mut server) = LocalSession::pair();

    let mut send = ready(client.poll_open_uni(&mut cx)).unwrap();
    ready(send.poll_write(&mut cx, b"data")).unwrap();

    let mut recv = ready(server.poll_accept_uni(&mut cx)).unwrap();

    // A zero-length slice, not a `BytesMut`: `BytesMut::chunk_mut` reserves more
    // capacity on demand, so it is never actually full.
    let mut full: &mut [u8] = &mut [];
    assert_eq!(
        ready(recv.poll_read_buf(&mut cx, &mut full)).unwrap(),
        Some(0),
        "a full buffer must not look like a finished stream"
    );
    assert_eq!(
        ready(recv.poll_read_chunk(&mut cx, 0)).unwrap(),
        Some(Bytes::new()),
        "asking for zero bytes must not look like a finished stream"
    );
}

/// The reason `poll_send_datagram` is pollable rather than infallible: a caller can
/// wait for room instead of having the payload silently dropped. The loopback queues
/// two, so the third parks until the peer drains one.
#[test]
fn send_datagram_reports_backpressure() {
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);

    let (mut client, mut server) = LocalSession::pair();
    let payload = Bytes::from_static(b"dgram");

    ready(client.poll_send_datagram(&mut cx, &payload)).unwrap();
    ready(client.poll_send_datagram(&mut cx, &payload)).unwrap();

    assert!(
        client.poll_send_datagram(&mut cx, &payload).is_pending(),
        "a full transport must park rather than drop the datagram"
    );

    // The peer drains one, so there is room again.
    ready(server.poll_recv_datagram(&mut cx)).unwrap();
    ready(client.poll_send_datagram(&mut cx, &payload)).unwrap();
}
