use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Buf;
use tokio::io::AsyncWrite;

use crate::{ez, waiters::Parked, StreamError};

// "send" in ascii; if you see this then call finish().await or close(code)
// hex: 0x73656E64, or 0x52E51B4DCE20 as an HTTP error code
// decimal: 1685221232, or 91143959072288 as an HTTP error code
const DROP_CODE: u64 = web_transport_proto::error_to_http3(0x73656E64);

/// A stream that can be used to send bytes.
///
/// This wrapper is mainly needed for error codes.
/// WebTransport uses u32 error codes and they're mapped in a reserved HTTP/3 error space.
pub struct SendStream {
    inner: ez::SendStream,

    // For the poll traits, which are handed a `Context` rather than a waiter. One cell
    // covers both write methods: they are the same operation.
    parked_write: Parked,
    parked_closed: Parked,
}

impl SendStream {
    pub(super) fn new(inner: ez::SendStream) -> Self {
        Self {
            inner,
            parked_write: Parked::default(),
            parked_closed: Parked::default(),
        }
    }

    /// Write some data to the stream, returning the size written.
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, StreamError> {
        self.inner.write(buf).await.map_err(Into::into)
    }

    /// Write data from a buffer to the stream, returning the size written.
    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<usize, StreamError> {
        self.inner.write_buf(buf).await.map_err(Into::into)
    }

    /// Write all of the data to the stream.
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), StreamError> {
        self.inner.write_all(buf).await.map_err(Into::into)
    }

    /// Write all data from a buffer to the stream.
    pub async fn write_buf_all<B: Buf>(&mut self, buf: &mut B) -> Result<(), StreamError> {
        self.inner.write_buf_all(buf).await.map_err(Into::into)
    }

    /// Mark the stream as finished, such that no more data can be written.
    pub fn finish(&mut self) -> Result<(), StreamError> {
        self.inner.finish().map_err(Into::into)
    }

    /// Set the priority of this stream.
    ///
    /// Streams with a higher priority are sent first, but are not guaranteed to arrive first.
    /// Defaults to 0. This matches the W3C WebTransport `sendOrder` convention and the other
    /// `web-transport` backends.
    ///
    /// See [`ez::SendStream::set_priority`] for how the `i32` is ranked onto quiche's
    /// 8-bit stream urgency.
    pub fn set_priority(&mut self, order: i32) {
        self.inner.set_priority(order)
    }

    /// Abruptly reset the stream with the provided error code.
    ///
    /// This is a u32 with WebTransport because it shares the error space with HTTP/3.
    pub fn reset(&mut self, code: u32) {
        let code = web_transport_proto::error_to_http3(code);
        self.inner.reset(code)
    }

    /// Wait until the stream has been stopped and return the error code.
    pub async fn closed(&mut self) -> Result<(), StreamError> {
        self.inner.closed().await.map_err(Into::into)
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        // Reset the stream if we dropped without calling `close` or `reset`
        if !self.inner.is_finished().unwrap_or(true) {
            tracing::warn!("stream dropped without `close` or `reset`");
            self.inner.reset(DROP_CODE)
        }
    }
}

impl AsyncWrite for SendStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let inner = std::pin::pin!(&mut self.inner);
        inner.poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let inner = std::pin::pin!(&mut self.inner);
        inner.poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        let inner = std::pin::pin!(&mut self.inner);
        inner.poll_shutdown(cx)
    }
}

impl web_transport_trait::SendStream for SendStream {
    type Error = StreamError;

    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.write(buf).await
    }

    fn set_priority(&mut self, order: i32) {
        SendStream::set_priority(self, order)
    }

    fn reset(&mut self, code: u32) {
        self.reset(code)
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        self.finish()
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        self.closed().await
    }
}

impl web_transport_trait::poll::SendStream for SendStream {
    type Error = StreamError;

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
        self.parked_write
            .poll(cx, |waiter| self.inner.poll_write(waiter, buf))
            .map_err(Into::into)
    }

    fn poll_write_buf<B: Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<usize, Self::Error>> {
        // ez advances `buf` by exactly what it accepted, so this keeps the
        // partial-write contract while skipping the default's extra copy.
        self.parked_write
            .poll(cx, |waiter| self.inner.poll_write_buf(waiter, buf))
            .map_err(Into::into)
    }

    fn set_priority(&mut self, order: i32) {
        SendStream::set_priority(self, order)
    }

    fn reset(&mut self, code: u32) {
        self.reset(code)
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        self.finish()
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.parked_closed
            .poll(cx, |waiter| self.inner.poll_closed(waiter))
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn priority(stream: &SendStream) -> i32 {
        stream
            .inner
            .priority()
            .expect("priority is set on construction")
    }

    #[test]
    fn untouched_stream_uses_the_default_priority() {
        let stream = SendStream::new(ez::SendStream::new_test());
        assert_eq!(priority(&stream), 0);
    }

    #[test]
    fn async_trait_matches_the_inherent_api() {
        let mut inherent = SendStream::new(ez::SendStream::new_test());
        let mut via_trait = SendStream::new(ez::SendStream::new_test());

        inherent.set_priority(200);
        web_transport_trait::SendStream::set_priority(&mut via_trait, 200);

        assert_eq!(priority(&via_trait), priority(&inherent));
        assert_eq!(priority(&via_trait), 200);
    }

    #[test]
    fn poll_trait_matches_the_inherent_api() {
        let mut inherent = SendStream::new(ez::SendStream::new_test());
        let mut via_trait = SendStream::new(ez::SendStream::new_test());

        inherent.set_priority(100);
        web_transport_trait::poll::SendStream::set_priority(&mut via_trait, 100);

        assert_eq!(priority(&via_trait), priority(&inherent));
        assert_eq!(priority(&via_trait), 100);
    }
}
