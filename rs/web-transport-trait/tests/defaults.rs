//! Tests for the traits' provided methods.
//!
//! The streams here accept or produce one byte per call, so a partial transfer is
//! the normal case rather than an edge case. That is what the `_all` helpers exist
//! to paper over, and getting it wrong is invisible on a stream that happens to
//! take everything in one go.

use std::{
    future::Future,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use bytes::Bytes;
use web_transport_trait::SendStream;

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

    async fn write(&mut self, buf: &[u8]) -> Result<usize, TestError> {
        match buf.first() {
            Some(&byte) => {
                self.written.push(byte);
                Ok(1)
            }
            None => Ok(0),
        }
    }

    fn set_priority(&mut self, _order: u8) {}

    fn finish(&mut self) -> Result<(), TestError> {
        Ok(())
    }

    fn reset(&mut self, _code: u32) {}

    async fn closed(&mut self) -> Result<(), TestError> {
        Ok(())
    }
}

/// `write_chunk` promises the whole chunk. Built on a single `write_buf` it stopped
/// at whatever the first call accepted and dropped the rest silently, which the peer
/// decodes as a truncated frame.
#[test]
fn write_chunk_writes_the_whole_chunk() {
    let mut send = DripSend::default();
    block_on(send.write_chunk(Bytes::from_static(b"hello"))).unwrap();
    assert_eq!(send.written, b"hello");
}

#[test]
fn write_all_drains_the_buffer() {
    let mut send = DripSend::default();
    block_on(send.write_all(b"hello")).unwrap();
    assert_eq!(send.written, b"hello");
}

#[test]
fn write_all_buf_drains_the_buffer() {
    let mut send = DripSend::default();
    let mut buf = Bytes::from_static(b"hello");
    block_on(send.write_all_buf(&mut buf)).unwrap();
    assert_eq!(send.written, b"hello");
    assert!(buf.is_empty());
}

#[test]
fn write_buf_advances_only_by_what_was_accepted() {
    let mut send = DripSend::default();
    let mut buf = Bytes::from_static(b"hello");
    let size = block_on(send.write_buf(&mut buf)).unwrap();
    assert_eq!(size, 1);
    assert_eq!(&buf[..], b"ello");
}
