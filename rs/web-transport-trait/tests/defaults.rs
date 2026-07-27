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
use web_transport_trait::{RecvStream, SendStream};

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

impl SendStream for DripSend {
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

/// Produces at most one byte per call.
struct DripRecv {
    remaining: Bytes,
}

impl RecvStream for DripRecv {
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
