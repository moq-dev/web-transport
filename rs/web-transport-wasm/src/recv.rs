use std::{
    future::poll_fn,
    task::{ready, Context, Poll},
};

use bytes::{BufMut, Bytes};
use js_sys::Uint8Array;
use web_sys::{ReadableStreamDefaultReader, WebTransportReceiveStream};

use crate::{
    error::{stream_error, stream_error_code},
    js::{ignore, promise, read_value, Op},
    Error,
};

/// A stream of bytes received from the remote peer.
///
/// This can be closed by either side with an error code, or closed by the remote with a FIN.
pub struct RecvStream {
    reader: ReadableStreamDefaultReader,

    /// The `read()` in flight, retained so a caller that gives up on a poll does not
    /// discard the chunk it was about to receive.
    read: Op,
    closed: Op,

    /// Bytes taken from the browser but not yet handed to the caller.
    buffer: Bytes,

    /// The peer finished the stream and everything before the FIN has been read.
    fin: bool,
}

impl RecvStream {
    pub(super) fn new(stream: WebTransportReceiveStream) -> Result<Self, Error> {
        let reader = ReadableStreamDefaultReader::new(&stream)?;

        Ok(Self {
            reader,
            read: Op::default(),
            closed: Op::default(),
            buffer: Bytes::new(),
            fin: false,
        })
    }

    /// Poll for the next chunk of data, up to `max` bytes.
    ///
    /// Returns `None` once the peer has finished the stream.
    pub fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, Error>> {
        // Asking for no bytes is not end of stream.
        if max == 0 {
            return Poll::Ready(Ok(Some(Bytes::new())));
        }

        loop {
            if !self.buffer.is_empty() {
                let size = max.min(self.buffer.len());
                return Poll::Ready(Ok(Some(self.buffer.split_to(size))));
            }

            if self.fin {
                return Poll::Ready(Ok(None));
            }

            ready!(self.poll_fill(cx))?;
        }
    }

    /// Poll to read some data into `dst`, returning the number of bytes read.
    ///
    /// Returns `None` once the peer has finished the stream. An empty `dst` reads
    /// nothing and returns `Some(0)`.
    pub fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, Error>> {
        if dst.is_empty() {
            return Poll::Ready(Ok(Some(0)));
        }

        loop {
            if !self.buffer.is_empty() {
                let size = dst.len().min(self.buffer.len());
                dst[..size].copy_from_slice(&self.buffer.split_to(size));
                return Poll::Ready(Ok(Some(size)));
            }

            if self.fin {
                return Poll::Ready(Ok(None));
            }

            ready!(self.poll_fill(cx))?;
        }
    }

    /// Take the next chunk from the browser, or record the FIN.
    ///
    /// Only called with an empty buffer, so nothing is overwritten.
    fn poll_fill(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let reader = &self.reader;
        let result = ready!(self.read.poll(cx, || promise(reader.read())))?;

        match read_value::<Uint8Array>(result) {
            // TODO can we avoid making a copy here?
            Some(data) => self.buffer = data.to_vec().into(),
            None => self.fin = true,
        }

        Poll::Ready(Ok(()))
    }

    /// Poll until the stream has been closed, returning the error code if any.
    pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<u32>, Error>> {
        let err = match ready!(self.poll_closed_raw(cx)) {
            Ok(()) => return Poll::Ready(Ok(None)),
            Err(err) => err,
        };

        // If it's a WebTransportError, we can extract the error code.
        if let Error::Stream(err) = &err {
            if let Some(code) = stream_error_code(err) {
                return Poll::Ready(Ok(Some(code)));
            }
        }

        Poll::Ready(Err(err))
    }

    /// Poll until the stream has been closed by either side.
    ///
    /// The browser resolves the reader's `closed` promise once the FIN has been
    /// received *and* its queue drained, and rejects it on a reset or on our own
    /// cancel, which is the whole contract with no emulation needed.
    ///
    /// This can resolve while bytes we already pulled off that queue are still
    /// buffered here. Keep reading until [`poll_read`](Self::poll_read) returns
    /// `None`: closed describes the transport, not what the caller has consumed,
    /// and the buffered bytes are still delivered afterwards. Deferring instead
    /// would deadlock, since this borrows `&mut self` and so a caller waiting here
    /// is a caller who cannot read.
    pub(crate) fn poll_closed_raw(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let reader = &self.reader;
        ready!(self.closed.poll(cx, || promise(reader.closed())))?;
        Poll::Ready(Ok(()))
    }

    /// Read the next chunk of data with the provided maximum size.
    ///
    /// This returns a chunk of data instead of copying, which may be more efficient.
    pub async fn read(&mut self, max: usize) -> Result<Option<Bytes>, Error> {
        poll_fn(|cx| self.poll_read_chunk(cx, max)).await
    }

    /// Read some data into the provided buffer.
    ///
    /// Returns the (non-zero) number of bytes read, or None if the stream is closed.
    /// Advances the buffer by the number of bytes read.
    pub async fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Result<Option<usize>, Error> {
        let chunk = match self.read(buf.remaining_mut()).await? {
            Some(chunk) if !chunk.is_empty() => chunk,
            _ => return Ok(None),
        };

        let size = chunk.len();
        buf.put(chunk);

        Ok(Some(size))
    }

    /// Send a `STOP_SENDING` QUIC code, informing the peer that no more data will
    /// be read.
    pub fn stop(&mut self, code: u32) {
        ignore(self.reader.cancel_with_reason(&stream_error(code)));
    }

    /// Block until the stream has been closed and return the error code, if any.
    pub async fn closed(&mut self) -> Result<Option<u32>, Error> {
        poll_fn(|cx| self.poll_closed(cx)).await
    }
}

impl Drop for RecvStream {
    /// Release flow control, which the peer never gets back otherwise.
    fn drop(&mut self) {
        self.stop(0);
    }
}
