use std::{
    future::poll_fn,
    task::{ready, Context, Poll},
};

use bytes::Buf;
use js_sys::{Reflect, Uint8Array};
use web_sys::{WebTransportSendStream, WritableStreamDefaultWriter};

use crate::{
    error::stream_error,
    js::{ignore, promise, Op},
    Error,
};

/// A stream of bytes sent to the remote peer.
pub struct SendStream {
    stream: WebTransportSendStream,
    writer: WritableStreamDefaultWriter,

    /// The chunk handed to the browser but not yet accepted. Its bytes were already
    /// reported to the caller, so waiting on it costs the caller nothing.
    write: Op,
    closed: Op,

    /// `finish` or `reset` was called, so no further writes can be accepted.
    terminal: bool,
}

impl SendStream {
    pub(super) fn new(stream: WebTransportSendStream) -> Result<Self, Error> {
        let writer = WritableStreamDefaultWriter::new(&stream)?;

        Ok(Self {
            stream,
            writer,
            write: Op::default(),
            closed: Op::default(),
            terminal: false,
        })
    }

    /// Poll to write `buf` to the stream, returning how many bytes were written.
    ///
    /// The browser's writer takes a chunk whole, so this accepts all of `buf` at
    /// once. Backpressure comes from the *previous* chunk, waited on before this
    /// call looks at `buf` — which is what lets a [`Poll::Pending`] here leave the
    /// caller's buffer untouched, even if they come back with a different one.
    pub fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
        if self.terminal {
            return Poll::Ready(Err(Error::Closed));
        }

        ready!(self.write.poll_settled(cx))?;

        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        let chunk = Uint8Array::from(buf);
        self.write
            .start(promise(self.writer.write_with_chunk(&chunk)));

        Poll::Ready(Ok(buf.len()))
    }

    /// Poll until the stream has been closed, returning the error code if any.
    pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<u8>, Error>> {
        let err = match ready!(self.poll_closed_raw(cx)) {
            Ok(()) => return Poll::Ready(Ok(None)),
            Err(err) => err,
        };

        // If it's a WebTransportError, we can extract the error code.
        if let Error::Stream(err) = &err {
            if let Some(code) = err.stream_error_code() {
                return Poll::Ready(Ok(Some(code)));
            }
        }

        Poll::Ready(Err(err))
    }

    /// Poll until the stream is closed by either side.
    ///
    /// The browser resolves the writer's `closed` promise once our FIN is through
    /// and rejects it on a reset or a peer STOP_SENDING. Nothing here owns the
    /// stream, so watching for that does not block a concurrent write.
    pub(crate) fn poll_closed_raw(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let writer = &self.writer;
        ready!(self.closed.poll(cx, || promise(writer.closed())))?;
        Poll::Ready(Ok(()))
    }

    /// Write *all* of the given bytes to the stream.
    pub async fn write(&mut self, buf: &[u8]) -> Result<(), Error> {
        poll_fn(|cx| self.poll_write(cx, buf)).await?;
        Ok(())
    }

    /// Writes some of the given buffer to the stream.
    pub async fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Result<usize, Error> {
        let size = poll_fn(|cx| {
            let chunk = buf.chunk();
            self.poll_write(cx, chunk)
        })
        .await?;

        buf.advance(size);
        Ok(size)
    }

    /// Send an immediate reset code, closing the stream with an error.
    pub fn reset(&mut self, code: u32) {
        self.terminal = true;
        ignore(self.writer.abort_with_reason(&stream_error(code)));
    }

    /// Mark the stream as finished.
    ///
    /// The browser flushes whatever is already queued before the FIN, so this does
    /// not race a write still in flight.
    ///
    /// This is automatically called on Drop, but can be called manually.
    pub fn finish(&mut self) -> Result<(), Error> {
        if self.terminal {
            return Ok(());
        }

        self.terminal = true;
        ignore(self.writer.close());
        Ok(())
    }

    /// Set the stream's priority.
    ///
    /// Streams with **higher** values are sent first, but are not guaranteed to arrive first.
    pub fn set_priority(&mut self, priority: i32) {
        Reflect::set(&self.stream, &"sendOrder".into(), &priority.into())
            .expect("failed to set priority");
    }

    /// Block until the stream has been closed and return the error code, if any.
    pub async fn closed(&mut self) -> Result<Option<u8>, Error> {
        poll_fn(|cx| self.poll_closed(cx)).await
    }
}

impl Drop for SendStream {
    /// Close the stream with a FIN.
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
