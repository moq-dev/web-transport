use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Buf, Bytes};
use iroh::endpoint;

use crate::{ClosedStream, SessionError, WriteError};

/// A stream that can be used to send bytes. See [`iroh::endpoint::SendStream`].
///
/// This wrapper is mainly needed for error codes, which is unfortunate.
/// WebTransport uses u32 error codes and they're mapped in a reserved HTTP/3 error space.
#[derive(Debug)]
pub struct SendStream {
    stream: endpoint::SendStream,
}

impl SendStream {
    pub(crate) fn new(stream: endpoint::SendStream) -> Self {
        Self { stream }
    }

    /// Abruptly reset the stream with the provided error code. See [`iroh::endpoint::SendStream::reset`].
    /// This is a u32 with WebTransport because we share the error space with HTTP/3.
    pub fn reset(&mut self, code: u32) -> Result<(), ClosedStream> {
        let code = web_transport_proto::error_to_http3(code);
        let code = endpoint::VarInt::try_from(code).unwrap();
        self.stream.reset(code).map_err(Into::into)
    }

    /// Wait until the stream has been stopped and return the error code. See [`iroh::endpoint::SendStream::stopped`].
    ///
    /// Unlike Quinn, this returns None if the code is not a valid WebTransport error code.
    /// Also unlike Quinn, this returns a SessionError, not a StoppedError, because 0-RTT is not supported.
    pub async fn stopped(&mut self) -> Result<Option<u32>, SessionError> {
        match self.stream.stopped().await {
            Ok(Some(code)) => Ok(web_transport_proto::error_from_http3(code.into_inner())),
            Ok(None) => Ok(None),
            Err(endpoint::StoppedError::ConnectionLost(e)) => Err(e.into()),
            Err(endpoint::StoppedError::ZeroRttRejected) => {
                unreachable!("0-RTT not supported")
            }
        }
    }

    // Unfortunately, we have to wrap WriteError for a bunch of functions.

    /// Write some data to the stream, returning the size written. See [`iroh::endpoint::SendStream::write`].
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, WriteError> {
        self.stream.write(buf).await.map_err(Into::into)
    }

    /// Write all of the data to the stream. See [`iroh::endpoint::SendStream::write_all`].
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), WriteError> {
        self.stream.write_all(buf).await.map_err(Into::into)
    }

    /// Write chunks of data to the stream. See [`iroh::endpoint::SendStream::write_chunks`].
    pub async fn write_chunks(
        &mut self,
        bufs: &mut [Bytes],
    ) -> Result<endpoint::Written, WriteError> {
        self.stream.write_chunks(bufs).await.map_err(Into::into)
    }

    /// Write a chunk of data to the stream. See [`iroh::endpoint::SendStream::write_chunk`].
    pub async fn write_chunk(&mut self, buf: Bytes) -> Result<(), WriteError> {
        self.stream.write_chunk(buf).await.map_err(Into::into)
    }

    /// Write all of the chunks of data to the stream. See [`iroh::endpoint::SendStream::write_all_chunks`].
    pub async fn write_all_chunks(&mut self, bufs: &mut [Bytes]) -> Result<(), WriteError> {
        self.stream.write_all_chunks(bufs).await.map_err(Into::into)
    }

    /// Mark the stream as finished, such that no more data can be written. See [`iroh::endpoint::SendStream::finish`].
    pub fn finish(&mut self) -> Result<(), ClosedStream> {
        self.stream.finish().map_err(Into::into)
    }

    /// Set the stream's priority. See [`iroh::endpoint::SendStream::set_priority`].
    pub fn set_priority(&self, order: i32) -> Result<(), ClosedStream> {
        self.stream.set_priority(order).map_err(Into::into)
    }

    /// Returns the stream's current priority. See [`iroh::endpoint::SendStream::priority`].
    pub fn priority(&self) -> Result<i32, ClosedStream> {
        self.stream.priority().map_err(Into::into)
    }
}

impl tokio::io::AsyncWrite for SendStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // We have to use this syntax because iroh::endpoint added its own poll_write method.
        tokio::io::AsyncWrite::poll_write(Pin::new(&mut self.stream), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl web_transport_trait::SendStream for SendStream {
    type Error = WriteError;

    fn set_priority(&mut self, order: u8) {
        self.stream.set_priority(order.into()).ok();
    }

    fn reset(&mut self, code: u32) {
        Self::reset(self, code).ok();
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        Self::finish(self).map_err(|_| WriteError::ClosedStream)?;
        Ok(())
    }

    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        Self::write(self, buf).await
    }

    async fn write_buf<B: Buf + Send>(&mut self, buf: &mut B) -> Result<usize, Self::Error> {
        // This can avoid making a copy when Buf is Bytes, as Quinn will allocate anyway.
        let size = buf.chunk().len();
        let chunk = buf.copy_to_bytes(size);
        self.write_chunk(chunk).await?;
        Ok(size)
    }

    async fn write_chunk(&mut self, chunk: Bytes) -> Result<(), Self::Error> {
        self.write_chunk(chunk).await
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        self.stopped().await?;
        Ok(())
    }
}
