use std::{
    io,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
};

use bytes::Bytes;

use crate::{ReadError, ReadExactError, ReadToEndError, SessionError};

/// A stream that can be used to recieve bytes. See [`noq::RecvStream`].
#[derive(Debug)]
pub struct RecvStream {
    inner: noq::RecvStream,
    error: Arc<OnceLock<SessionError>>,
}

impl RecvStream {
    pub(crate) fn new(stream: noq::RecvStream, error: Arc<OnceLock<SessionError>>) -> Self {
        Self {
            inner: stream,
            error,
        }
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
        self.inner.read(buf).await.map_err(|e| self.map_error(e))
    }

    /// Fill the entire buffer with data. See [`noq::RecvStream::read_exact`].
    pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadExactError> {
        self.inner.read_exact(buf).await.map_err(|e| match e {
            noq::ReadExactError::ReadError(e) => self.map_error(e).into(),
            e => e.into(),
        })
    }

    /// Read a chunk of data from the stream. See [`noq::RecvStream::read_chunk`].
    pub async fn read_chunk(&mut self, max_length: usize) -> Result<Option<noq::Chunk>, ReadError> {
        self.inner
            .read_chunk(max_length)
            .await
            .map_err(|e| self.map_error(e))
    }

    /// Read chunks of data from the stream. See [`noq::RecvStream::read_chunks`].
    pub async fn read_chunks(&mut self, bufs: &mut [Bytes]) -> Result<Option<usize>, ReadError> {
        self.inner
            .read_chunks(bufs)
            .await
            .map_err(|e| self.map_error(e))
    }

    /// Read until the end of the stream or the limit is hit. See [`noq::RecvStream::read_to_end`].
    pub async fn read_to_end(&mut self, size_limit: usize) -> Result<Vec<u8>, ReadToEndError> {
        self.inner
            .read_to_end(size_limit)
            .await
            .map_err(|e| match e {
                noq::ReadToEndError::Read(e) => self.map_error(e).into(),
                e => e.into(),
            })
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

    // We purposely don't expose the 0RTT because it's not valid with WebTransport
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
        self.read_chunk(max)
            .await
            .map(|r| r.map(|chunk| chunk.bytes))
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        self.received_reset().await?;
        Ok(())
    }
}
