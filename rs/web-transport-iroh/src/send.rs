use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Buf, Bytes};
use iroh::endpoint;

use crate::{
    ClosedStream, SessionError, WriteError,
    error::{decode_stream_code, encode_stream_code},
};

/// A stream that can be used to send bytes. See [`iroh::endpoint::SendStream`].
///
/// This wrapper is mainly needed for error codes, which is unfortunate.
/// WebTransport uses u32 error codes and they're mapped in a reserved HTTP/3 error space.
#[derive(Debug)]
pub struct SendStream {
    stream: endpoint::SendStream,
    // Raw QUIC carries stream codes as is; HTTP/3 maps them into its own code space.
    raw: bool,
}

impl SendStream {
    pub(crate) fn new(stream: endpoint::SendStream, raw: bool) -> Self {
        Self { stream, raw }
    }

    /// Decode the peer's stop code with the session's code space.
    fn map_error(&self, e: endpoint::WriteError) -> WriteError {
        match e {
            endpoint::WriteError::Stopped(code) => match decode_stream_code(code, self.raw) {
                Some(code) => WriteError::Stopped(code),
                None => WriteError::InvalidStopped(code),
            },
            e => e.into(),
        }
    }

    /// Abruptly reset the stream with the provided error code. See [`iroh::endpoint::SendStream::reset`].
    /// This is a u32 with WebTransport because we share the error space with HTTP/3.
    /// A raw QUIC session sends the code as is.
    pub fn reset(&mut self, code: u32) -> Result<(), ClosedStream> {
        let code = encode_stream_code(code, self.raw);
        self.stream.reset(code).map_err(Into::into)
    }

    /// Wait until the stream has been stopped and return the error code. See [`iroh::endpoint::SendStream::stopped`].
    ///
    /// Unlike Quinn, this returns None if the code is not a valid WebTransport error code.
    /// Also unlike Quinn, this returns a SessionError, not a StoppedError, because 0-RTT is not supported.
    pub async fn stopped(&mut self) -> Result<Option<u32>, SessionError> {
        match self.stream.stopped().await {
            Ok(Some(code)) => Ok(decode_stream_code(code, self.raw)),
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
        self.stream.write(buf).await.map_err(|e| self.map_error(e))
    }

    /// Write all of the data to the stream. See [`iroh::endpoint::SendStream::write_all`].
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), WriteError> {
        self.stream
            .write_all(buf)
            .await
            .map_err(|e| self.map_error(e))
    }

    /// Write chunks of data to the stream, returning the number of bytes written.
    ///
    /// See [`iroh::endpoint::SendStream::write_many_chunks`].
    pub async fn write_many_chunks(
        &mut self,
        bufs: &mut &mut [Bytes],
    ) -> Result<usize, WriteError> {
        self.stream
            .write_many_chunks(bufs)
            .await
            .map_err(|e| self.map_error(e))
    }

    /// Write a chunk of data to the stream. See [`iroh::endpoint::SendStream::write_chunk`].
    pub async fn write_chunk(&mut self, buf: Bytes) -> Result<(), WriteError> {
        self.stream
            .write_chunk(buf)
            .await
            .map_err(|e| self.map_error(e))
    }

    /// Write all of the chunks of data to the stream. See [`iroh::endpoint::SendStream::write_all_chunks`].
    pub async fn write_all_chunks(&mut self, bufs: &mut [Bytes]) -> Result<(), WriteError> {
        self.stream
            .write_all_chunks(bufs)
            .await
            .map_err(|e| self.map_error(e))
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

    fn set_priority(&mut self, order: i32) {
        self.stream.set_priority(order).ok();
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

    async fn write_buf<B: Buf + web_transport_trait::MaybeSend>(
        &mut self,
        buf: &mut B,
    ) -> Result<usize, Self::Error> {
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
        match self.stopped().await? {
            Some(code) => Err(WriteError::Stopped(code)),
            None => Ok(()),
        }
    }
}
