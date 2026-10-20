//! The safe read entry points may inspect every byte of their destination and
//! report a bad length. Neither behavior may make the provided helpers unsound.
use std::{
    collections::VecDeque,
    future::{poll_fn, Future},
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use bytes::{buf::UninitSlice, BufMut, Bytes, BytesMut};
use web_transport_trait::{poll, RecvStream};

#[derive(Debug, PartialEq)]
struct TestError;

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("test error")
    }
}
impl std::error::Error for TestError {}
impl web_transport_trait::Error for TestError {
    fn session_error(&self) -> Option<(u32, String)> {
        None
    }
}

#[derive(Clone, Copy)]
enum Surface {
    Async,
    Poll,
}
const SURFACES: [Surface; 2] = [Surface::Async, Surface::Poll];

#[derive(Default)]
struct Reader {
    data: VecDeque<u8>,
    calls: usize,
    destination_len: usize,
    pending: bool,
    zero: bool,
    invalid: bool,
    error: bool,
}

impl Reader {
    fn new(data: &[u8]) -> Self {
        Self {
            data: data.iter().copied().collect(),
            ..Self::default()
        }
    }
}

impl poll::RecvStream for Reader {
    type Error = TestError;

    fn poll_read(
        &mut self,
        _cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, TestError>> {
        self.calls += 1;
        self.destination_len = dst.len();
        // Inspecting the entire safe slice is legal, even before writing to it.
        std::hint::black_box(dst.iter().fold(0u8, |sum, byte| sum ^ byte));
        if self.pending {
            self.pending = false;
            dst.fill(0x55);
            return Poll::Pending;
        }
        if self.error {
            dst.fill(0x55);
            return Poll::Ready(Err(TestError));
        }
        if self.invalid {
            return Poll::Ready(Ok(Some(dst.len() + 1)));
        }
        if self.zero || dst.is_empty() {
            return Poll::Ready(Ok(Some(0)));
        }
        Poll::Ready(Ok(self.data.pop_front().map(|byte| {
            dst[0] = byte;
            1
        })))
    }

    fn stop(&mut self, _code: u32) {}
    fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), TestError>> {
        Poll::Ready(Ok(()))
    }
}

impl RecvStream for Reader {
    type Error = TestError;

    async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, TestError> {
        poll_fn(|cx| poll::RecvStream::poll_read(self, cx, dst)).await
    }
    fn stop(&mut self, _code: u32) {}
    async fn closed(&mut self) -> Result<(), TestError> {
        Ok(())
    }
}

struct Noop;
impl Wake for Noop {
    fn wake(self: Arc<Self>) {}
}
fn with_context<T>(f: impl FnOnce(&mut Context<'_>) -> T) -> T {
    let waker = Waker::from(Arc::new(Noop));
    f(&mut Context::from_waker(&waker))
}
fn poll_once<F: Future>(future: F, cx: &mut Context<'_>) -> Poll<F::Output> {
    std::pin::pin!(future).as_mut().poll(cx)
}
fn read_buf<B: BufMut + Send>(
    surface: Surface,
    reader: &mut Reader,
    buf: &mut B,
) -> Poll<Result<Option<usize>, TestError>> {
    with_context(|cx| match surface {
        Surface::Async => poll_once(reader.read_buf(buf), cx),
        Surface::Poll => poll::RecvStream::poll_read_buf(reader, cx, buf),
    })
}
fn read_chunk(
    surface: Surface,
    reader: &mut Reader,
    max: usize,
) -> Poll<Result<Option<Bytes>, TestError>> {
    with_context(|cx| match surface {
        Surface::Async => poll_once(reader.read_chunk(max), cx),
        Surface::Poll => poll::RecvStream::poll_read_chunk(reader, cx, max),
    })
}

fn inspect_buffer_destination(surface: Surface) {
    let mut reader = Reader::new(b"ab");
    let mut buf = BytesMut::with_capacity(8);
    buf.extend_from_slice(b"prefix");
    for size in [1, 1] {
        assert_eq!(
            read_buf(surface, &mut reader, &mut buf),
            Poll::Ready(Ok(Some(size)))
        );
    }
    assert_eq!(&buf[..], b"prefixab");
    assert_eq!(
        read_buf(surface, &mut reader, &mut buf),
        Poll::Ready(Ok(None))
    );
    assert_eq!(&buf[..], b"prefixab");
}

fn inspect_chunk_destination(surface: Surface) {
    let mut reader = Reader::new(b"ab");
    assert_eq!(
        read_chunk(surface, &mut reader, 1),
        Poll::Ready(Ok(Some(Bytes::from_static(b"a"))))
    );
    assert_eq!(reader.destination_len, 1);
    assert_eq!(
        read_chunk(surface, &mut reader, usize::MAX),
        Poll::Ready(Ok(Some(Bytes::from_static(b"b"))))
    );
    assert_eq!(read_chunk(surface, &mut reader, 8), Poll::Ready(Ok(None)));
}

#[test]
fn safe_async_readers_can_inspect_buffer_destinations() {
    inspect_buffer_destination(Surface::Async);
}
#[test]
fn safe_poll_readers_can_inspect_buffer_destinations() {
    inspect_buffer_destination(Surface::Poll);
}
#[test]
fn safe_async_readers_can_inspect_chunk_destinations() {
    inspect_chunk_destination(Surface::Async);
}
#[test]
fn safe_poll_readers_can_inspect_chunk_destinations() {
    inspect_chunk_destination(Surface::Poll);
}

#[test]
fn empty_requests_do_not_read_or_report_eof() {
    for surface in SURFACES {
        let mut reader = Reader::new(b"a");
        let mut dst: &mut [u8] = &mut [];
        assert_eq!(
            read_buf(surface, &mut reader, &mut dst),
            Poll::Ready(Ok(Some(0)))
        );
        assert_eq!(
            read_chunk(surface, &mut reader, 0),
            Poll::Ready(Ok(Some(Bytes::new())))
        );
        assert_eq!(reader.calls, 0);
        assert_eq!(
            read_chunk(surface, &mut reader, 1),
            Poll::Ready(Ok(Some(Bytes::from_static(b"a"))))
        );
    }
}

#[test]
fn zero_progress_is_distinct_from_eof() {
    for surface in SURFACES {
        let mut reader = Reader {
            zero: true,
            ..Reader::default()
        };
        let mut buf = BytesMut::with_capacity(8);
        assert_eq!(
            read_buf(surface, &mut reader, &mut buf),
            Poll::Ready(Ok(Some(0)))
        );
        assert!(buf.is_empty());
        assert_eq!(
            read_chunk(surface, &mut reader, 8),
            Poll::Ready(Ok(Some(Bytes::new())))
        );
    }
}

#[test]
fn pending_and_cancelled_reads_leave_buffer_progress_unchanged() {
    for surface in SURFACES {
        let mut reader = Reader {
            pending: true,
            ..Reader::new(b"a")
        };
        let mut buf = BytesMut::with_capacity(8);
        buf.extend_from_slice(b"prefix");
        // The async future is dropped after its first pending poll.
        assert!(read_buf(surface, &mut reader, &mut buf).is_pending());
        assert_eq!(&buf[..], b"prefix");
        assert_eq!(
            read_buf(surface, &mut reader, &mut buf),
            Poll::Ready(Ok(Some(1)))
        );
        assert_eq!(&buf[..], b"prefixa");

        let mut reader = Reader {
            pending: true,
            ..Reader::new(b"b")
        };
        assert!(read_chunk(surface, &mut reader, 8).is_pending());
        assert_eq!(
            read_chunk(surface, &mut reader, 8),
            Poll::Ready(Ok(Some(Bytes::from_static(b"b"))))
        );
    }
}

#[test]
fn errors_leave_buffer_progress_unchanged() {
    for surface in SURFACES {
        let mut reader = Reader {
            error: true,
            ..Reader::default()
        };
        let mut buf = BytesMut::with_capacity(8);
        buf.extend_from_slice(b"prefix");
        assert_eq!(
            read_buf(surface, &mut reader, &mut buf),
            Poll::Ready(Err(TestError))
        );
        assert_eq!(&buf[..], b"prefix");
        assert_eq!(
            read_chunk(surface, &mut reader, 8),
            Poll::Ready(Err(TestError))
        );
    }
}

struct TrackedBuf {
    inner: BytesMut,
    advanced: bool,
}

// SAFETY: All buffer operations delegate to BytesMut with the same bounds.
unsafe impl BufMut for TrackedBuf {
    fn remaining_mut(&self) -> usize {
        self.inner.remaining_mut()
    }

    fn chunk_mut(&mut self) -> &mut UninitSlice {
        self.inner.chunk_mut()
    }

    unsafe fn advance_mut(&mut self, count: usize) {
        self.advanced = true;
        // SAFETY: The caller must satisfy the same contract as BytesMut.
        unsafe { self.inner.advance_mut(count) };
    }
}

#[test]
fn invalid_lengths_panic_before_advancing_buffers() {
    for surface in SURFACES {
        let mut reader = Reader {
            invalid: true,
            ..Reader::default()
        };
        let mut buf = TrackedBuf {
            inner: BytesMut::with_capacity(8),
            advanced: false,
        };
        buf.inner.extend_from_slice(b"prefix");
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            read_buf(surface, &mut reader, &mut buf)
        }));
        assert!(panic.is_err());
        assert!(
            !buf.advanced,
            "invalid lengths must not reach unsafe advancement"
        );
        assert_eq!(&buf.inner[..], b"prefix");
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            read_chunk(surface, &mut reader, 8)
        }));
        assert!(panic.is_err());
    }
}
