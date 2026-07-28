//! Tests for the traits' provided methods.
//!
//! The streams here accept or produce one byte per poll, so a partial transfer is
//! the normal case rather than an edge case. That is what the `_all` helpers exist
//! to paper over, and getting it wrong is invisible on a stream that happens to
//! take everything in one call.

use std::{
    future::Future,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use bytes::{BufMut, Bytes, BytesMut};
use web_transport_trait::{PollRecvStream, PollSendStream, PollSession, RecvStream, SendStream};

struct Noop;

impl Wake for Noop {
    fn wake(self: Arc<Self>) {}
}

/// Drive a future to completion. Sound only because nothing in this file ever
/// returns `Pending`; a real executor would park instead of spinning.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);

    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
    }
}

#[derive(Debug)]
struct TestError;

impl std::fmt::Display for TestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "test error")
    }
}

impl std::error::Error for TestError {}

impl web_transport_trait::Error for TestError {
    fn session_error(&self) -> Option<(u32, String)> {
        None
    }
}

/// Accepts at most one byte per call.
#[derive(Default)]
struct DripSend {
    written: Vec<u8>,
}

impl PollSendStream for DripSend {
    type Error = TestError;

    fn poll_write(&mut self, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, TestError>> {
        match buf.first() {
            Some(&byte) => {
                self.written.push(byte);
                Poll::Ready(Ok(1))
            }
            None => Poll::Ready(Ok(0)),
        }
    }

    fn set_priority(&mut self, _order: u8) {}

    fn finish(&mut self) -> Result<(), TestError> {
        Ok(())
    }

    fn reset(&mut self, _code: u32) {}

    fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), TestError>> {
        Poll::Ready(Ok(()))
    }
}

impl SendStream for DripSend {}

/// Produces at most one byte per call.
struct DripRecv {
    remaining: Bytes,
}

impl PollRecvStream for DripRecv {
    type Error = TestError;

    fn poll_read(
        &mut self,
        _cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, TestError>> {
        if self.remaining.is_empty() || dst.is_empty() {
            return Poll::Ready(Ok(None));
        }

        dst[0] = self.remaining[0];
        self.remaining = self.remaining.slice(1..);
        Poll::Ready(Ok(Some(1)))
    }

    fn stop(&mut self, _code: u32) {}

    fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), TestError>> {
        Poll::Ready(Ok(()))
    }
}

impl RecvStream for DripRecv {}

/// `write_chunk` takes ownership of the chunk and promises to write all of it, so
/// it cannot be built on a single `write_buf` — that returns after one partial
/// write and drops the rest on the floor, truncating the stream with no error.
#[test]
fn write_chunk_writes_the_whole_chunk() {
    let mut stream = DripSend::default();
    block_on(stream.write_chunk(Bytes::from_static(b"hello"))).unwrap();
    assert_eq!(stream.written, b"hello");
}

#[test]
fn write_all_writes_the_whole_slice() {
    let mut stream = DripSend::default();
    block_on(stream.write_all(b"hello")).unwrap();
    assert_eq!(stream.written, b"hello");
}

#[test]
fn write_all_buf_drains_the_buffer() {
    let mut stream = DripSend::default();
    let mut buf = Bytes::from_static(b"hello");
    block_on(stream.write_all_buf(&mut buf)).unwrap();
    assert_eq!(stream.written, b"hello");
    assert!(buf.is_empty());
}

/// A single `write_buf` is allowed to be partial, and must advance the buffer by
/// exactly what it took.
#[test]
fn write_buf_reports_and_advances_by_what_it_took() {
    let mut stream = DripSend::default();
    let mut buf = Bytes::from_static(b"hello");
    assert_eq!(block_on(stream.write_buf(&mut buf)).unwrap(), 1);
    assert_eq!(stream.written, b"h");
    assert_eq!(&buf[..], b"ello");
}

#[test]
fn read_all_reads_until_the_stream_ends() {
    let mut stream = DripRecv {
        remaining: Bytes::from_static(b"hello"),
    };
    assert_eq!(&block_on(stream.read_all()).unwrap()[..], b"hello");
}

/// `read_chunk` is a single read, so one byte at a time is a valid answer here.
#[test]
fn read_chunk_returns_what_is_available() {
    let mut stream = DripRecv {
        remaining: Bytes::from_static(b"hello"),
    };
    assert_eq!(&block_on(stream.read_chunk(16)).unwrap().unwrap()[..], b"h");
}

#[test]
fn read_chunk_reports_end_of_stream() {
    let mut stream = DripRecv {
        remaining: Bytes::new(),
    };
    assert!(block_on(stream.read_chunk(16)).unwrap().is_none());
}

#[test]
fn read_all_buf_fills_the_buffer_then_stops() {
    let mut stream = DripRecv {
        remaining: Bytes::from_static(b"hello"),
    };
    let mut buf = BytesMut::with_capacity(3).limit(3);
    assert_eq!(block_on(stream.read_all_buf(&mut buf)).unwrap(), 3);
    assert_eq!(&buf.into_inner()[..], b"hel");
}

/// The reason the poll surface is its own trait: a `!Send` type can implement it.
/// A thread-per-core runtime — `io_uring`, say — whose streams never leave the
/// thread that owns them is then expressible, and drivable from a poll loop. It
/// cannot implement [`SendStream`], which needs `Send` to hand out `Send` futures,
/// and it does not need to.
///
/// This is a compile-time claim; there is nothing to assert at runtime.
#[test]
fn a_not_send_type_can_implement_the_poll_surface() {
    struct ThreadBound {
        _rc: std::rc::Rc<()>,
    }

    impl PollSendStream for ThreadBound {
        type Error = TestError;

        fn poll_write(
            &mut self,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize, TestError>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn set_priority(&mut self, _order: u8) {}

        fn finish(&mut self) -> Result<(), TestError> {
            Ok(())
        }

        fn reset(&mut self, _code: u32) {}

        fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), TestError>> {
            Poll::Ready(Ok(()))
        }
    }

    let mut stream = ThreadBound {
        _rc: Default::default(),
    };
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);

    let Poll::Ready(Ok(size)) = stream.poll_write(&mut cx, b"hi") else {
        panic!("expected a completed write");
    };
    assert_eq!(size, 2);
}

/// Fills whatever destination it is handed, so the `max` bound has to come from
/// the caller rather than from the stream being polite.
struct FloodRecv;

impl PollRecvStream for FloodRecv {
    type Error = TestError;

    fn poll_read(
        &mut self,
        _cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, TestError>> {
        dst.fill(b'x');
        Poll::Ready(Ok(Some(dst.len())))
    }

    fn stop(&mut self, _code: u32) {}

    fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), TestError>> {
        Poll::Ready(Ok(()))
    }
}

impl RecvStream for FloodRecv {}

/// `read_chunk(max)` documents "up to the max size", and `BytesMut::with_capacity`
/// is free to over-allocate — so the default must bound the slice it hands out
/// rather than trusting the allocation to be exact.
#[test]
fn read_chunk_never_exceeds_max() {
    for max in [1usize, 3, 5, 7, 100, 1000, 4095] {
        let chunk = block_on(FloodRecv.read_chunk(max)).unwrap().unwrap();
        assert!(
            chunk.len() <= max,
            "read_chunk({max}) returned {} bytes",
            chunk.len()
        );
    }
}

/// The reason `PollSession` is its own trait: a `!Send` session can implement it,
/// so a thread-per-core runtime is expressible end to end rather than only for its
/// streams. It cannot implement [`Session`], which needs `Send` to hand out `Send`
/// futures, and does not need to.
///
/// This is a compile-time claim; there is nothing to assert at runtime.
#[test]
fn a_not_send_session_can_implement_the_poll_surface() {
    struct ThreadBound {
        _rc: std::rc::Rc<()>,
    }

    impl PollSession for ThreadBound {
        type SendStream = FloodSend;
        type RecvStream = FloodRecv;
        type Error = TestError;

        fn poll_accept_uni(&mut self, _cx: &mut Context<'_>) -> Poll<Result<FloodRecv, TestError>> {
            Poll::Pending
        }

        fn poll_accept_bi(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(FloodSend, FloodRecv), TestError>> {
            Poll::Pending
        }

        fn poll_open_uni(&mut self, _cx: &mut Context<'_>) -> Poll<Result<FloodSend, TestError>> {
            Poll::Pending
        }

        fn poll_open_bi(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(FloodSend, FloodRecv), TestError>> {
            Poll::Pending
        }

        fn send_datagram(&self, _payload: Bytes) -> Result<(), TestError> {
            Ok(())
        }

        fn poll_recv_datagram(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Bytes, TestError>> {
            Poll::Pending
        }

        fn max_datagram_size(&self) -> usize {
            0
        }

        fn close(&self, _code: u32, _reason: &str) {}

        fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<TestError> {
            Poll::Pending
        }
    }

    let session = ThreadBound {
        _rc: Default::default(),
    };
    assert_eq!(session.max_datagram_size(), 0);
}

/// Accepts every write, so it can stand in as a session's stream type.
struct FloodSend;

impl PollSendStream for FloodSend {
    type Error = TestError;

    fn poll_write(&mut self, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, TestError>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn set_priority(&mut self, _order: u8) {}

    fn finish(&mut self) -> Result<(), TestError> {
        Ok(())
    }

    fn reset(&mut self, _code: u32) {}

    fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), TestError>> {
        Poll::Ready(Ok(()))
    }
}

/// A safe `PollRecvStream` must not be able to cause unsoundness by over-reporting
/// its read. `poll_read_buf` feeds the count to `BufMut::advance_mut`, which is
/// unsafe, so the count is checked rather than trusted.
#[test]
#[should_panic(expected = "poll_read reported")]
fn over_reported_read_panics_rather_than_advancing_past_the_buffer() {
    struct Liar;

    impl PollRecvStream for Liar {
        type Error = TestError;

        fn poll_read(
            &mut self,
            _cx: &mut Context<'_>,
            dst: &mut [u8],
        ) -> Poll<Result<Option<usize>, TestError>> {
            // Claims to have written far more than it was given room for.
            Poll::Ready(Ok(Some(dst.len() + 4096)))
        }

        fn stop(&mut self, _code: u32) {}

        fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), TestError>> {
            Poll::Ready(Ok(()))
        }
    }

    impl RecvStream for Liar {}

    let mut buf = BytesMut::with_capacity(64);
    let _ = block_on(Liar.read_buf(&mut buf));
}

/// A destination with no room must not read as end of stream.
///
/// `None` means the peer finished; a full buffer means try again with space. The
/// backends draw that distinction in `poll_read`, and the defaults have to keep it
/// — collapsing the two turns "buffer full" into silent truncation.
#[test]
fn a_full_destination_is_not_end_of_stream() {
    let mut stream = DripRecv {
        remaining: Bytes::from_static(b"hello"),
    };

    let mut empty: &mut [u8] = &mut [];
    assert_eq!(block_on(stream.read_buf(&mut empty)).unwrap(), Some(0));

    // Still readable afterwards: nothing was consumed and nothing was closed.
    assert_eq!(&block_on(stream.read_chunk(16)).unwrap().unwrap()[..], b"h");
}

/// Asking for zero bytes is likewise not end of stream.
#[test]
fn a_zero_max_chunk_is_not_end_of_stream() {
    let mut stream = DripRecv {
        remaining: Bytes::from_static(b"hello"),
    };

    let chunk = block_on(stream.read_chunk(0)).unwrap();
    assert_eq!(chunk.as_deref(), Some(&[][..]));

    assert_eq!(&block_on(stream.read_chunk(16)).unwrap().unwrap()[..], b"h");
}
