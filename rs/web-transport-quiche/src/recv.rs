use std::{
    pin::{pin, Pin},
    task::{ready, Context, Poll},
};

use bytes::{BufMut, Bytes};
use tokio::io::{AsyncRead, ReadBuf};

use crate::{ez, StreamError};

// "recv" in ascii; if you see this then read everything or close(code)
// hex: 0x44454356, or 0x52E4EA9B7F80 as an HTTP error code
// decimal: 1146556178, or 91143142080384 as an HTTP error code
const DROP_CODE: u64 = web_transport_proto::error_to_http3(0x44454356);

/// A stream that can be used to receive bytes.
pub struct RecvStream {
    inner: ez::RecvStream,
}

impl RecvStream {
    pub(super) fn new(inner: ez::RecvStream) -> Self {
        Self { inner }
    }

    /// Read some data into the buffer and return the amount read.
    ///
    /// Returns `None` if the stream has been finished.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, StreamError> {
        self.inner.read(buf).await.map_err(Into::into)
    }

    /// Poll for some data and copy it into the buffer.
    pub fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<Result<Option<usize>, StreamError>> {
        let chunk =
            ready!(self.inner.poll_read_chunk(cx.waker(), buf.len())).map_err(StreamError::from)?;
        let size = chunk.map(|chunk| {
            buf[..chunk.len()].copy_from_slice(&chunk);
            chunk.len()
        });
        Poll::Ready(Ok(size))
    }

    /// Read a chunk of data from the stream.
    ///
    /// Returns `None` if the stream has been finished.
    pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, StreamError> {
        self.inner.read_chunk(max).await.map_err(Into::into)
    }

    /// Poll for the next chunk of data without copying.
    pub fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, StreamError>> {
        self.inner
            .poll_read_chunk(cx.waker(), max)
            .map(|result| result.map_err(Into::into))
    }

    /// Read data into a mutable buffer and return the amount read.
    ///
    /// Returns `None` if the stream has been finished.
    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<Option<usize>, StreamError> {
        self.inner.read_buf(buf).await.map_err(Into::into)
    }

    /// Read until the end of the stream or the limit is hit.
    pub async fn read_all(&mut self, max: usize) -> Result<Bytes, StreamError> {
        self.inner.read_all(max).await.map_err(Into::into)
    }

    /// Tell the other end to stop sending data with the given error code.
    ///
    /// This is a u32 with WebTransport since it shares the error space with HTTP/3.
    pub fn stop(&mut self, code: u32) {
        self.inner.stop(web_transport_proto::error_to_http3(code));
    }

    /// Block until the stream has been reset and return the error code.
    pub async fn closed(&mut self) -> Result<(), StreamError> {
        self.inner.closed().await.map_err(Into::into)
    }

    /// Poll until the stream is closed.
    pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), StreamError>> {
        self.inner
            .poll_closed(cx.waker())
            .map(|result| result.map_err(Into::into))
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        if !self.inner.is_closed() {
            tracing::warn!("stream dropped without `stop` or reading all contents");
            self.inner.stop(DROP_CODE)
        }
    }
}

impl AsyncRead for RecvStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let pinned = pin!(&mut self.inner);
        pinned.poll_read(cx, buf)
    }
}

impl web_transport_trait::RecvStream for RecvStream {
    type Error = StreamError;

    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, Self::Error>> {
        self.poll_read(cx, dst)
    }

    fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, Self::Error>> {
        self.poll_read_chunk(cx, max)
    }

    fn stop(&mut self, code: u32) {
        self.stop(code);
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_closed(cx)
    }
}
