use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use iroh::endpoint;

use crate::{
    ClosedStream, ReadError, ReadExactError, ReadToEndError, SessionError,
    error::{decode_stream_code, encode_stream_code},
};

/// A stream that can be used to receive bytes. See [`iroh::endpoint::RecvStream`].
#[derive(Debug)]
pub struct RecvStream {
    inner: endpoint::RecvStream,
    // Raw QUIC carries stream codes as is; HTTP/3 maps them into its own code space.
    raw: bool,
}

impl RecvStream {
    pub(crate) fn new(stream: endpoint::RecvStream, raw: bool) -> Self {
        Self { inner: stream, raw }
    }

    /// Decode the peer's reset code with the session's code space.
    fn map_error(&self, e: endpoint::ReadError) -> ReadError {
        match e {
            endpoint::ReadError::Reset(code) => match decode_stream_code(code, self.raw) {
                Some(code) => ReadError::Reset(code),
                None => ReadError::InvalidReset(code),
            },
            e => e.into(),
        }
    }

    /// Tell the other end to stop sending data with the given error code. See [`iroh::endpoint::RecvStream::stop`].
    /// This is a u32 with WebTransport since it shares the error space with HTTP/3.
    /// A raw QUIC session sends the code as is.
    pub fn stop(&mut self, code: u32) -> Result<(), endpoint::ClosedStream> {
        let code = encode_stream_code(code, self.raw);
        self.inner.stop(code)
    }

    // Unfortunately, we have to wrap ReadError for a bunch of functions.

    /// Read some data into the buffer and return the amount read. See [`iroh::endpoint::RecvStream::read`].
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ReadError> {
        self.inner.read(buf).await.map_err(|e| self.map_error(e))
    }

    /// Fill the entire buffer with data. See [`iroh::endpoint::RecvStream::read_exact`].
    pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadExactError> {
        self.inner.read_exact(buf).await.map_err(|e| match e {
            endpoint::ReadExactError::ReadError(e) => self.map_error(e).into(),
            e => e.into(),
        })
    }

    /// Read a chunk of data from the stream. See [`iroh::endpoint::RecvStream::read_chunk`].
    pub async fn read_chunk(&mut self, max_length: usize) -> Result<Option<Bytes>, ReadError> {
        self.inner
            .read_chunk(max_length)
            .await
            .map_err(|e| self.map_error(e))
    }

    /// Read chunks of data from the stream. See [`iroh::endpoint::RecvStream::read_many_chunks`].
    pub async fn read_many_chunks(
        &mut self,
        bufs: &mut [Bytes],
    ) -> Result<Option<usize>, ReadError> {
        self.inner
            .read_many_chunks(bufs)
            .await
            .map_err(|e| self.map_error(e))
    }

    /// Read until the end of the stream or the limit is hit. See [`iroh::endpoint::RecvStream::read_to_end`].
    pub async fn read_to_end(&mut self, size_limit: usize) -> Result<Vec<u8>, ReadToEndError> {
        self.inner
            .read_to_end(size_limit)
            .await
            .map_err(|e| match e {
                endpoint::ReadToEndError::Read(e) => self.map_error(e).into(),
                e => e.into(),
            })
    }

    /// Block until the stream has been reset and return the error code. See [`iroh::endpoint::RecvStream::received_reset`].
    ///
    /// Unlike Quinn, this returns a SessionError, not a ResetError, because 0-RTT is not supported.
    pub async fn received_reset(&mut self) -> Result<Option<u32>, SessionError> {
        match self.inner.received_reset().await {
            Ok(None) => Ok(None),
            Ok(Some(code)) => Ok(decode_stream_code(code, self.raw)),
            Err(endpoint::ResetError::ConnectionLost(e)) => Err(e.into()),
            Err(endpoint::ResetError::ZeroRttRejected) => unreachable!("0-RTT not supported"),
        }
    }

    /// Returns the number of bytes read from this stream.
    ///
    /// This is the offset of the next byte to be read, i.e. the length of the contiguous
    /// prefix of the stream consumed by the application.
    pub fn bytes_read(&self) -> Result<u64, ClosedStream> {
        self.inner.bytes_read().map_err(|_| ClosedStream)
    }

    // We purposely don't expose the stream ID or 0RTT because it's not valid with WebTransport
}

impl tokio::io::AsyncRead for RecvStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl web_transport_trait::RecvStream for RecvStream {
    type Error = ReadError;

    fn stop(&mut self, code: u32) {
        Self::stop(self, code).ok();
    }

    async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
        self.read(dst).await
    }

    async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Self::Error> {
        self.read_chunk(max).await
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        self.received_reset().await?;
        Ok(())
    }
}
