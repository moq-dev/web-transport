use std::{
    io,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
};

use bytes::Bytes;

use crate::{ClosedStream, ReadError, ReadExactError, ReadToEndError, SessionError};

/// A stream that can be used to receive bytes. See [`noq::RecvStream`].
#[derive(Debug)]
pub struct RecvStream {
    bytes_read: u64,
    inner: noq::RecvStream,
    error: Arc<OnceLock<SessionError>>,
}

impl RecvStream {
    pub(crate) fn new(stream: noq::RecvStream, error: Arc<OnceLock<SessionError>>) -> Self {
        Self {
            inner: stream,
            bytes_read: 0,
            error,
        }
    }

    /// Collect application payload statistics for this stream.
    pub async fn stats(&mut self) -> Result<web_transport_trait::RecvStreamStats, ReadError> {
        let mut stats = web_transport_trait::RecvStreamStats::default();
        stats.bytes_read = Some(self.bytes_read);
        Ok(stats)
    }

    /// Replace connection-level errors with the stored session error if available.
    fn map_error(&self, e: impl Into<ReadError>) -> ReadError {
        let e = e.into();
        if let Some(err) = self.error.get() {
            if matches!(&e, ReadError::SessionError(_)) {
                return ReadError::SessionError(err.clone());
            }
        }
        e
    }

    /// Tell the other end to stop sending data with the given error code. See [`noq::RecvStream::stop`].
    /// This is a u32 with WebTransport since it shares the error space with HTTP/3.
    pub fn stop(&mut self, code: u32) -> Result<(), noq::ClosedStream> {
        let code = web_transport_proto::error_to_http3(code);
        let code = noq::VarInt::try_from(code).unwrap();
        self.inner.stop(code)
    }

    // Unfortunately, we have to wrap ReadError for a bunch of functions.

    /// Read some data into the buffer and return the amount read. See [`noq::RecvStream::read`].
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ReadError> {
        let size = self.inner.read(buf).await.map_err(|e| self.map_error(e))?;
        self.bytes_read += size.unwrap_or(0) as u64;
        Ok(size)
    }

    /// Fill the entire buffer with data. See [`noq::RecvStream::read_exact`].
    pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadExactError> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.read(&mut buf[filled..]).await? {
                Some(size) => filled += size,
                None => return Err(ReadExactError::FinishedEarly(filled)),
            }
        }
        Ok(())
    }

    /// Read a chunk of data from the stream. See [`noq::RecvStream::read_chunk`].
    pub async fn read_chunk(&mut self, max_length: usize) -> Result<Option<Bytes>, ReadError> {
        let chunk = self
            .inner
            .read_chunk(max_length)
            .await
            .map_err(|e| self.map_error(e))?;
        self.bytes_read += chunk.as_ref().map_or(0, |chunk| chunk.len()) as u64;
        Ok(chunk)
    }

    /// Read chunks of data from the stream. See [`noq::RecvStream::read_many_chunks`].
    pub async fn read_many_chunks(
        &mut self,
        bufs: &mut [Bytes],
    ) -> Result<Option<usize>, ReadError> {
        let count = self
            .inner
            .read_many_chunks(bufs)
            .await
            .map_err(|e| self.map_error(e))?;
        self.bytes_read += bufs[..count.unwrap_or(0)]
            .iter()
            .map(|chunk| chunk.len() as u64)
            .sum::<u64>();
        Ok(count)
    }

    /// Read until the end of the stream or the limit is hit. See [`noq::RecvStream::read_to_end`].
    pub async fn read_to_end(&mut self, size_limit: usize) -> Result<Vec<u8>, ReadToEndError> {
        let mut data = Vec::new();
        while let Some(chunk) = self.read_chunk(usize::MAX).await? {
            if chunk.len() > size_limit - data.len() {
                return Err(ReadToEndError::TooLong);
            }
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }

    /// Block until the stream has been reset and return the error code. See [`noq::RecvStream::received_reset`].
    ///
    /// Unlike Noq, this returns a SessionError, not a ResetError, because 0-RTT is not supported.
    pub async fn received_reset(&mut self) -> Result<Option<u32>, SessionError> {
        match self.inner.received_reset().await {
            Ok(None) => Ok(None),
            Ok(Some(code)) => Ok(web_transport_proto::error_from_http3(code.into_inner())),
            Err(noq::ResetError::ConnectionLost(conn_err)) => {
                Err(self.error.get().cloned().unwrap_or_else(|| conn_err.into()))
            }
            Err(noq::ResetError::ZeroRttRejected) => unreachable!("0-RTT not supported"),
        }
    }

    /// Return the underlying QUIC stream ID.
    ///
    /// > **Warning**
    /// >
    /// > WebTransport sessions share the QUIC connection with HTTP/3 and potentially other sessions.
    /// > The [noq::StreamId::index] might not increment by 1 like expected when using [noq].
    /// > This is why the Javascript WebTransport API does not expose the Stream ID.
    pub fn quic_id(&self) -> noq::StreamId {
        self.inner.id()
    }

    /// Return the native QUIC read offset, including any WebTransport stream header.
    ///
    /// This offset can become unavailable after transport closure. Use
    /// [`Self::stats`] for the cumulative application payload count without headers,
    /// which remains available after closure.
    pub fn bytes_read(&self) -> Result<u64, ClosedStream> {
        self.inner.bytes_read().map_err(|_| ClosedStream)
    }

    // We purposely don't expose the 0RTT because it's not valid with WebTransport
}

impl tokio::io::AsyncRead for RecvStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        self.bytes_read += (buf.filled().len() - before) as u64;
        result
    }
}

impl web_transport_trait::RecvStream for RecvStream {
    type Error = ReadError;

    async fn stats(&mut self) -> Result<web_transport_trait::RecvStreamStats, Self::Error> {
        self.stats().await
    }

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
