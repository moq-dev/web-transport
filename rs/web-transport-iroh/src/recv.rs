use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use iroh::endpoint;

use crate::{ReadError, ReadExactError, ReadToEndError, SessionError};

/// A stream that can be used to receive bytes. See [`iroh::endpoint::RecvStream`].
#[derive(Debug)]
pub struct RecvStream {
    inner: endpoint::RecvStream,
}

impl RecvStream {
    pub(crate) fn new(stream: endpoint::RecvStream) -> Self {
        Self { inner: stream }
    }

    /// Tell the other end to stop sending data with the given error code. See [`iroh::endpoint::RecvStream::stop`].
    /// This is a u32 with WebTransport since it shares the error space with HTTP/3.
    pub fn stop(&mut self, code: u32) -> Result<(), endpoint::ClosedStream> {
        let code = web_transport_proto::error_to_http3(code);
        let code = endpoint::VarInt::try_from(code).unwrap();
        self.inner.stop(code)
    }

    // Unfortunately, we have to wrap ReadError for a bunch of functions.

    /// Read some data into the buffer and return the amount read. See [`iroh::endpoint::RecvStream::read`].
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ReadError> {
        self.inner.read(buf).await.map_err(Into::into)
    }

    /// Fill the entire buffer with data. See [`iroh::endpoint::RecvStream::read_exact`].
    pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadExactError> {
        self.inner.read_exact(buf).await.map_err(Into::into)
    }

    /// Read a chunk of data from the stream. See [`iroh::endpoint::RecvStream::read_chunk`].
    pub async fn read_chunk(
        &mut self,
        max_length: usize,
    ) -> Result<Option<endpoint::Chunk>, ReadError> {
        self.inner.read_chunk(max_length).await.map_err(Into::into)
    }

    /// Read chunks of data from the stream. See [`iroh::endpoint::RecvStream::read_chunks`].
    pub async fn read_chunks(&mut self, bufs: &mut [Bytes]) -> Result<Option<usize>, ReadError> {
        self.inner.read_chunks(bufs).await.map_err(Into::into)
    }

    /// Read until the end of the stream or the limit is hit. See [`iroh::endpoint::RecvStream::read_to_end`].
    pub async fn read_to_end(&mut self, size_limit: usize) -> Result<Vec<u8>, ReadToEndError> {
        self.inner.read_to_end(size_limit).await.map_err(Into::into)
    }

    /// Block until the stream has been reset and return the error code. See [`iroh::endpoint::RecvStream::received_reset`].
    ///
    /// Unlike Quinn, this returns a SessionError, not a ResetError, because 0-RTT is not supported.
    pub async fn received_reset(&mut self) -> Result<Option<u32>, SessionError> {
        match self.inner.received_reset().await {
            Ok(None) => Ok(None),
            Ok(Some(code)) => Ok(Some(
                web_transport_proto::error_from_http3(code.into_inner()).unwrap(),
            )),
            Err(endpoint::ResetError::ConnectionLost(e)) => Err(e.into()),
            Err(endpoint::ResetError::ZeroRttRejected) => unreachable!("0-RTT not supported"),
        }
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
        self.read_chunk(max)
            .await
            .map(|r| r.map(|chunk| chunk.bytes))
    }

    async fn closed(&mut self) -> Result<(), Self::Error> {
        self.received_reset().await?;
        Ok(())
    }
}
